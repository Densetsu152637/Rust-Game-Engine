//! Loading `terrain` project assets (shared by the runtime, project validation and MCP tools).
use crate::{Error, Project, Result};
use engine_terrain::Heightfield;

/// Converts a terrain-crate failure into a project diagnostic with the same code.
pub fn mapped(error: engine_terrain::Error) -> Error {
    Error::new(error.code, error.message)
}

/// Loads and validates the `terrain` asset `id` (the bounded binary format of
/// `engine_terrain::asset`).
pub fn load_terrain(project: &Project, id: &str) -> Result<Heightfield> {
    let asset = project.require_asset(id, "terrain")?;
    let path = crate::confined_file(&project.root, &asset.path)?;
    let bytes = crate::schema::read_bounded(&path, engine_terrain::asset::MAX_FILE_BYTES as u64)?;
    engine_terrain::asset::decode(&bytes)
        .map_err(|e| Error::new(e.code, format!("terrain asset {id}: {}", e.message)))
}
