//! Bounded CPU loading of externally prepared image-based lighting and lightmaps.
//! Cube faces and lightmaps contain linear RGB32F texels; no convolution,
//! UV unwrapping or baking occurs here.
use crate::{
    Error, Project, Result,
    schema::{confined_file, read_bounded},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::PathBuf,
    sync::Arc,
};

const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
const MAX_DESCRIPTORS_BYTES: u64 = 16 * MAX_DESCRIPTOR_BYTES;
pub const MAX_INDIRECT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_EDGE: u32 = 256;
const MAX_ENVIRONMENTS: usize = 17;
/// Matches the native indirect set limits (`MAX_LIGHTMAPS`, `MAX_RADIANCE_EDGE`).
pub const MAX_LIGHTMAPS: usize = 8;
const MAX_LIGHTMAP_EDGE: u32 = 2048;
const MAX_PROBES: usize = 16;
const MAX_HALF: f32 = 65_504.0;
const MAX_OPEN_FACES: usize = 4096;

fn invalid(message: impl Into<String>) -> Error {
    Error::new("asset.indirect.invalid", message)
}

#[derive(Clone, Debug)]
pub struct RadianceFace {
    pub size: u32,
    pub pixels: Vec<[f32; 3]>,
}
/// A supplied baked lightmap: linear RGB32F incident diffuse radiance
/// (irradiance / pi, exposure applied), top row first. Addressed by mesh UV2.
#[derive(Clone, Debug)]
pub struct LightmapImage {
    pub size: [u32; 2],
    pub pixels: Vec<[f32; 3]>,
}
#[derive(Clone, Debug)]
pub struct IndirectEnvironment {
    pub diffuse: [[f32; 3]; 9],
    pub mips: Vec<[RadianceFace; 6]>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentLightingAsset {
    pub environment: usize,
    pub rotation: [f32; 4],
    pub intensity: f32,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IndirectProbeAsset {
    pub id: String,
    pub position_absolute: [f64; 3],
    pub scale: [f32; 3],
    pub half_extents: [f32; 3],
    pub blend_distance: f32,
    pub priority: i32,
    pub lighting: EnvironmentLightingAsset,
}
#[derive(Clone, Debug)]
pub struct IndirectAsset {
    pub environments: Vec<IndirectEnvironment>,
    pub global: Option<EnvironmentLightingAsset>,
    pub probes: Vec<IndirectProbeAsset>,
    /// Supplied lightmaps, indexed by `MeshLightmap.lightmap`.
    pub lightmaps: Vec<LightmapImage>,
    pub decoded_bytes: u64,
    pub texture_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    version: u32,
    #[serde(default)]
    environments: Vec<EnvironmentSource>,
    global: Option<EnvironmentLightingAsset>,
    #[serde(default)]
    probes: Vec<IndirectProbeAsset>,
    #[serde(default)]
    lightmaps: Vec<LightmapSource>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LightmapSource {
    size: [u32; 2],
    path: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentSource {
    edge: u32,
    diffuse: [[f32; 3]; 9],
    mips: Vec<[PathBuf; 6]>,
}
struct PreparedSource {
    source: Source,
    faces: Vec<[PreparedFace; 6]>,
    lightmaps: Vec<PreparedFace>,
    decoded_bytes: u64,
    texture_bytes: u64,
}
struct PreparedFace {
    path: PathBuf,
    file: fs::File,
}

fn lighting_ok(value: &EnvironmentLightingAsset, count: usize) -> bool {
    let norm = value.rotation.iter().map(|v| v * v).sum::<f32>();
    value.environment < count
        && value.rotation.iter().all(|v| v.is_finite())
        && (norm - 1.0).abs() <= 1e-3
        && value.intensity.is_finite()
        && (0.0..=1000.0).contains(&value.intensity)
}

/// Preflight *all* descriptor and payload lengths before decoding any RGB data.
pub fn load_indirect_assets(project: &Project) -> Result<BTreeMap<String, Arc<IndirectAsset>>> {
    let mut prepared = Vec::new();
    let mut descriptor_total = 0u64;
    let mut decoded_total = 0u64;
    let mut open_faces = 0usize;
    for asset in project.assets.values().filter(|a| a.kind == "indirect") {
        let descriptor = project.resolve_asset(&asset.id)?;
        let bytes = read_bounded(&descriptor, MAX_DESCRIPTOR_BYTES)?;
        descriptor_total = descriptor_total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("descriptor size overflow"))?;
        if descriptor_total > MAX_DESCRIPTORS_BYTES {
            return Err(invalid("indirect descriptors exceed 16 MiB"));
        }
        let source: Source = serde_json::from_slice(&bytes)?;
        if source.version != 1
            || source.environments.len() > MAX_ENVIRONMENTS
            || source.probes.len() > MAX_PROBES
            || source.lightmaps.len() > MAX_LIGHTMAPS
        {
            return Err(invalid(
                "version must be 1, with at most 17 environments, 16 probes and 8 lightmaps",
            ));
        }
        if source.global.is_none() && source.probes.is_empty() && source.lightmaps.is_empty() {
            return Err(invalid(
                "indirect set needs a global environment, a local probe or a lightmap",
            ));
        }
        if source
            .global
            .as_ref()
            .is_some_and(|v| !lighting_ok(v, source.environments.len()))
        {
            return Err(invalid("invalid global environment lighting"));
        }
        let mut ids = BTreeSet::new();
        for probe in &source.probes {
            if probe.id.is_empty()
                || probe.id.len() > 64
                || !ids.insert(&probe.id)
                || !lighting_ok(&probe.lighting, source.environments.len())
                || !probe
                    .position_absolute
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 1e12)
                || !probe
                    .scale
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= 1e6)
                || f64::from(probe.scale[0]) * f64::from(probe.scale[1]) * f64::from(probe.scale[2])
                    <= 1e-8
                || !probe
                    .half_extents
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0 && *v <= 1e6)
                || !probe.blend_distance.is_finite()
                || probe.blend_distance < 0.0
                || probe.blend_distance
                    > probe
                        .half_extents
                        .iter()
                        .copied()
                        .fold(f32::INFINITY, f32::min)
            {
                return Err(invalid(format!("invalid indirect probe {}", probe.id)));
            }
        }
        let mut prepared_faces = Vec::new();
        let mut decoded_bytes = 0u64;
        let mut texture_bytes = 0u64;
        for environment in &source.environments {
            if environment.edge == 0
                || environment.edge > MAX_EDGE
                || !environment.edge.is_power_of_two()
                || environment.mips.len() != environment.edge.ilog2() as usize + 1
                || !environment
                    .diffuse
                    .iter()
                    .flatten()
                    .all(|v| v.is_finite() && v.abs() <= MAX_HALF)
            {
                return Err(invalid(
                    "invalid indirect environment dimensions, mips or diffuse coefficients",
                ));
            }
            for (level, faces) in environment.mips.iter().enumerate() {
                open_faces = open_faces
                    .checked_add(6)
                    .ok_or_else(|| invalid("face count overflow"))?;
                if open_faces > MAX_OPEN_FACES {
                    return Err(invalid(
                        "indirect assets reference more than 4096 face files",
                    ));
                }
                let edge = environment.edge >> level;
                let expected = u64::from(edge) * u64::from(edge) * 12;
                let gpu_bytes = u64::from(edge) * u64::from(edge) * 8;
                let resolved: [PreparedFace; 6] = faces
                    .iter()
                    .map(|face| -> Result<PreparedFace> {
                        let path = confined_file(&project.root, face)?;
                        let file = fs::File::open(&path)?;
                        if file.metadata()?.len() != expected {
                            return Err(invalid(format!(
                                "indirect face {face:?} needs exactly {expected} bytes"
                            )));
                        }
                        Ok(PreparedFace { path, file })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .try_into()
                    .map_err(|_| invalid("six cube faces required"))?;
                decoded_bytes = decoded_bytes
                    .checked_add(6 * expected)
                    .ok_or_else(|| invalid("indirect size overflow"))?;
                texture_bytes = texture_bytes
                    .checked_add(6 * gpu_bytes)
                    .ok_or_else(|| invalid("indirect size overflow"))?;
                prepared_faces.push(resolved);
            }
        }
        let mut lightmaps = Vec::new();
        for lightmap in &source.lightmaps {
            let [width, height] = lightmap.size;
            if width == 0 || height == 0 || width > MAX_LIGHTMAP_EDGE || height > MAX_LIGHTMAP_EDGE
            {
                return Err(invalid(format!(
                    "lightmap {:?} dimensions must be 1..={MAX_LIGHTMAP_EDGE}",
                    lightmap.path
                )));
            }
            open_faces = open_faces
                .checked_add(1)
                .ok_or_else(|| invalid("file count overflow"))?;
            if open_faces > MAX_OPEN_FACES {
                return Err(invalid(
                    "indirect assets reference more than 4096 face and lightmap files",
                ));
            }
            let texels = u64::from(width) * u64::from(height);
            let expected = texels * 12;
            let path = confined_file(&project.root, &lightmap.path)?;
            let file = fs::File::open(&path)?;
            if file.metadata()?.len() != expected {
                return Err(invalid(format!(
                    "lightmap {:?} needs exactly {expected} bytes for {width}x{height} RGB32F",
                    lightmap.path
                )));
            }
            decoded_bytes = decoded_bytes
                .checked_add(expected)
                .ok_or_else(|| invalid("indirect size overflow"))?;
            texture_bytes = texture_bytes
                .checked_add(texels * 8)
                .ok_or_else(|| invalid("indirect size overflow"))?;
            lightmaps.push(PreparedFace { path, file });
        }
        if decoded_bytes + 64 * 64 * 8 > MAX_INDIRECT_BYTES
            || texture_bytes + 64 * 64 * 8 > MAX_INDIRECT_BYTES
        {
            return Err(invalid("indirect set exceeds native 128 MiB budget"));
        }
        decoded_total = decoded_total
            .checked_add(decoded_bytes)
            .ok_or_else(|| invalid("indirect project size overflow"))?;
        if decoded_total > MAX_INDIRECT_BYTES {
            return Err(invalid("indirect project payloads exceed 128 MiB"));
        }
        prepared.push((
            asset.id.clone(),
            PreparedSource {
                source,
                faces: prepared_faces,
                lightmaps,
                decoded_bytes,
                texture_bytes,
            },
        ));
    }
    let mut result = BTreeMap::new();
    for (id, mut prepared) in prepared {
        let mut path_index = 0;
        let mut environments = Vec::new();
        for environment in prepared.source.environments {
            let mut mips = Vec::new();
            for level in 0..environment.mips.len() {
                let edge = environment.edge >> level;
                let faces = &mut prepared.faces[path_index];
                path_index += 1;
                let faces: [RadianceFace; 6] = faces
                    .iter_mut()
                    .map(|face| -> Result<RadianceFace> {
                        let expected = u64::from(edge) * u64::from(edge) * 12;
                        let mut bytes = Vec::new();
                        (&mut face.file)
                            .take(expected + 1)
                            .read_to_end(&mut bytes)?;
                        if bytes.len() as u64 != u64::from(edge) * u64::from(edge) * 12 {
                            return Err(invalid(format!(
                                "indirect face changed size during load: {:?}",
                                face.path
                            )));
                        }
                        let pixels = bytes
                            .as_chunks::<12>()
                            .0
                            .iter()
                            .map(|p| {
                                [
                                    f32::from_le_bytes(p[0..4].try_into().unwrap()),
                                    f32::from_le_bytes(p[4..8].try_into().unwrap()),
                                    f32::from_le_bytes(p[8..12].try_into().unwrap()),
                                ]
                            })
                            .collect::<Vec<_>>();
                        if !pixels
                            .iter()
                            .flatten()
                            .all(|v| v.is_finite() && (0.0..=MAX_HALF).contains(v))
                        {
                            return Err(invalid(format!(
                                "nonfinite or out-of-range radiance in {:?}",
                                face.path
                            )));
                        }
                        Ok(RadianceFace { size: edge, pixels })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .try_into()
                    .map_err(|_| invalid("six cube faces required"))?;
                mips.push(faces);
            }
            environments.push(IndirectEnvironment {
                diffuse: environment.diffuse,
                mips,
            });
        }
        let mut lightmaps = Vec::new();
        for (spec, file) in prepared
            .source
            .lightmaps
            .iter()
            .zip(&mut prepared.lightmaps)
        {
            let expected = u64::from(spec.size[0]) * u64::from(spec.size[1]) * 12;
            let mut bytes = Vec::new();
            (&mut file.file)
                .take(expected + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != expected {
                return Err(invalid(format!(
                    "lightmap changed size during load: {:?}",
                    file.path
                )));
            }
            let pixels = bytes
                .as_chunks::<12>()
                .0
                .iter()
                .map(|p| {
                    [
                        f32::from_le_bytes(p[0..4].try_into().unwrap()),
                        f32::from_le_bytes(p[4..8].try_into().unwrap()),
                        f32::from_le_bytes(p[8..12].try_into().unwrap()),
                    ]
                })
                .collect::<Vec<_>>();
            if !pixels
                .iter()
                .flatten()
                .all(|v| v.is_finite() && (0.0..=MAX_HALF).contains(v))
            {
                return Err(invalid(format!(
                    "nonfinite or out-of-range radiance in {:?}",
                    file.path
                )));
            }
            lightmaps.push(LightmapImage {
                size: spec.size,
                pixels,
            });
        }
        result.insert(
            id,
            Arc::new(IndirectAsset {
                environments,
                global: prepared.source.global,
                probes: prepared.source.probes,
                lightmaps,
                decoded_bytes: prepared.decoded_bytes,
                texture_bytes: prepared.texture_bytes,
            }),
        );
    }
    Ok(result)
}
