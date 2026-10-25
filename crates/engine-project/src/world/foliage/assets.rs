//! Loading foliage project assets: the `foliage` instance file, meshes, textures and density
//! textures.
use crate::content::{Mesh, Texture};
use crate::{Error, Project, Result};
use engine_foliage::{DensityGrid, DensityMap, FoliageAsset, FoliageType};
use std::sync::Arc;

/// Converts a foliage-crate failure into a project diagnostic with the same code.
pub fn mapped(error: engine_foliage::Error) -> Error {
    Error::new(error.code, error.message)
}

/// Loads and validates the `foliage` asset `id` (the bounded binary format of
/// `engine_foliage::asset`).
pub fn load_foliage(project: &Project, id: &str) -> Result<FoliageAsset> {
    let asset = project.require_asset(id, "foliage")?;
    let path = crate::confined_file(&project.root, &asset.path)?;
    let bytes = crate::schema::read_bounded(&path, engine_foliage::asset::MAX_FILE_BYTES as u64)?;
    engine_foliage::asset::decode(&bytes)
        .map_err(|e| Error::new(e.code, format!("foliage asset {id}: {}", e.message)))
}

pub(super) fn load_mesh(project: &Project, id: &str) -> Result<Arc<Mesh>> {
    project.require_asset(id, "mesh")?;
    Ok(Arc::new(crate::content::load_mesh(
        &project.resolve_asset(id)?,
    )?))
}

pub(super) fn load_texture(project: &Project, id: &str) -> Result<Arc<Texture>> {
    project.require_asset(id, "texture")?;
    Ok(Arc::new(crate::content::load_texture(
        &project.resolve_asset(id)?,
    )?))
}

/// Replaces every `DensityMap::Texture` by a `DensityMap::Grid` sampled from the texture's
/// red channel (at most 1024 x 1024 cells; nearest-neighbour when the texture is larger).
pub(super) fn resolve_density_maps(project: &Project, types: &mut [FoliageType]) -> Result<()> {
    for t in types {
        let DensityMap::Texture {
            asset,
            origin,
            size,
        } = &t.density_map
        else {
            continue;
        };
        let texture = load_texture(project, asset)?;
        let (w, h) = (texture.width.max(1), texture.height.max(1));
        if texture.pixels.len() < (w * h * 4) as usize {
            return Err(Error::new(
                "foliage.density.texture",
                format!("density texture {asset} has no RGBA pixels"),
            ));
        }
        let columns = w.min(engine_foliage::types::MAX_GRID_EDGE);
        let cell = size[0] / columns as f32;
        let rows = ((size[1] / cell).round() as u32).clamp(1, engine_foliage::types::MAX_GRID_EDGE);
        let mut values = Vec::with_capacity(columns as usize * rows as usize);
        for row in 0..rows {
            let ty = ((row as f32 + 0.5) / rows as f32 * h as f32) as u32;
            for column in 0..columns {
                let tx = ((column as f32 + 0.5) / columns as f32 * w as f32) as u32;
                let at = ((ty.min(h - 1) * w + tx.min(w - 1)) * 4) as usize;
                values.push(f32::from(texture.pixels[at]) / 255.0);
            }
        }
        t.density_map = DensityMap::Grid(DensityGrid {
            origin: *origin,
            cell_size: cell,
            columns,
            rows,
            values,
        });
    }
    Ok(())
}

/// The decoded render assets of one type.
#[derive(Clone, Debug)]
pub struct LoadedType {
    /// LOD 0 first.
    pub meshes: Vec<Arc<Mesh>>,
    pub texture: Option<Arc<Texture>>,
    pub impostor: Option<Arc<Texture>>,
}

/// The decoded render assets of a type list; `types` is the list they were built from.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub types: Vec<FoliageType>,
    pub data: Vec<LoadedType>,
}

impl Loaded {
    /// Loads every mesh and texture the types reference.
    pub fn load(project: &Project, types: &[FoliageType]) -> Result<Self> {
        let mut data = Vec::with_capacity(types.len());
        for t in types {
            let mut meshes = vec![load_mesh(project, &t.mesh)?];
            for m in &t.lod_meshes {
                meshes.push(load_mesh(project, m)?);
            }
            data.push(LoadedType {
                meshes,
                texture: t
                    .texture
                    .as_deref()
                    .map(|id| load_texture(project, id))
                    .transpose()?,
                impostor: t
                    .impostor
                    .as_ref()
                    .map(|i| load_texture(project, &i.texture))
                    .transpose()?,
            });
        }
        Ok(Self {
            types: types.to_vec(),
            data,
        })
    }
}
