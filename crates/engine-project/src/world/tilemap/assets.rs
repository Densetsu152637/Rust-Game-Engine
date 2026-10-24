//! Loading `tileset` and `tilemap` project assets (shared by the runtime and MCP tools).
use crate::{Error, Project, Result};
use engine_tilemap::{Tilemap, Tileset};

const MAX_ASSET_BYTES: u64 = 16 * 1024 * 1024;

/// Converts a tilemap-crate failure into a project diagnostic with the same code.
pub(crate) fn mapped(error: engine_tilemap::Error) -> Error {
    Error::new(error.code, error.message)
}

/// Loads and validates the `tileset` asset `id`, including that its texture asset exists and
/// that every atlas rectangle fits the texture.
pub fn load_tileset(project: &Project, id: &str) -> Result<(Tileset, [u32; 2])> {
    let asset = project.require_asset(id, "tileset")?;
    let path = crate::confined_file(&project.root, &asset.path)?;
    let tileset = Tileset::from_json(&crate::schema::read_bounded(&path, MAX_ASSET_BYTES)?)
        .map_err(mapped)?;
    let size = texture_size(project, tileset.texture())?;
    tileset.validate_texture(size[0], size[1]).map_err(mapped)?;
    Ok((tileset, size))
}

/// Pixel size of the project `texture` asset.
pub fn texture_size(project: &Project, texture: &str) -> Result<[u32; 2]> {
    project.require_asset(texture, "texture")?;
    let path = project.resolve_asset(texture)?;
    // PNG headers give the size without decoding; other supported formats (PPM) decode fully.
    if let Ok((width, height)) = image::image_dimensions(&path) {
        return Ok([width, height]);
    }
    let decoded = crate::content::load_texture(&path)?;
    let (width, height) = (decoded.width, decoded.height);
    Ok([width, height])
}

/// Loads and validates the `tilemap` asset `id` against `tileset` (rule tiles are resolved).
pub fn load_tilemap(project: &Project, id: &str, tileset: &Tileset) -> Result<Tilemap> {
    let asset = project.require_asset(id, "tilemap")?;
    let path = crate::confined_file(&project.root, &asset.path)?;
    Tilemap::from_json(
        &crate::schema::read_bounded(&path, MAX_ASSET_BYTES)?,
        Some(tileset),
    )
    .map_err(mapped)
}
