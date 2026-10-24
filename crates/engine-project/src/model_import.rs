//! Shared authoring import used by the CLI and write-gated MCP tool.
//! New generated files are validated before the manifest is atomically replaced.
mod generate;
mod model;
mod transaction;

use crate::{Asset, Error, Project, Result, confined_file, schema::read_bounded};
use engine_assets::{ImportLimits, import_gltf};
pub use model::{ModelDocument, load_model};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct ModelImportResult {
    pub model: String,
    pub prefab: String,
    pub directory: PathBuf,
    pub assets: Vec<Asset>,
    pub fingerprint: String,
    pub manifest_revision: String,
    pub warnings: Vec<String>,
}

/// Imports source-relative glTF/GLB into newly named project assets.
/// `source` is project-relative. `prefix` is a portable filename/ID segment.
/// Existing output directories and manifest IDs are never overwritten.
pub fn import_model(project: &Project, source: &Path, prefix: &str) -> Result<ModelImportResult> {
    if prefix.is_empty()
        || prefix.len() > 64
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::new(
            "asset.import.prefix",
            "Prefix must be 1-64 ASCII letters, digits, hyphens or underscores",
        ));
    }
    let source_path = confined_file(&project.root, source)?;
    let _lock = transaction::import_lock(&project.root)?;
    let before = read_bounded(&project.manifest, 16 * 1024 * 1024)?;
    let current = Project::load(&project.manifest)?;
    if current.root != project.root || current.assets != project.assets {
        return Err(Error::new(
            "asset.import.conflict",
            "Project asset catalog changed; reload before importing",
        ));
    }
    let scene = import_gltf(&project.root, &source_path, &ImportLimits::default())
        .map_err(|e| Error::new("asset.import.invalid", e.to_string()).at(source, ""))?;
    let staging = tempfile::Builder::new()
        .prefix(".model-import-")
        .tempdir_in(&project.root)?;
    let relative = PathBuf::from("assets").join(format!("imported-{prefix}"));
    let generated = generate::generate(&current, &scene, prefix, &relative, staging.path())?;
    let mut manifest: serde_json::Value = serde_json::from_slice(&before)?;
    let assets = manifest
        .get_mut("assets")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| Error::new("asset.import.manifest", "Manifest assets must be an array"))?;
    assets.extend(
        generated
            .assets
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    let after = serde_json::to_vec_pretty(&manifest)?;
    if after.len() > 16 * 1024 * 1024 {
        return Err(Error::new(
            "asset.import.limit",
            "Updated manifest exceeds 16 MiB",
        ));
    }
    transaction::publish(
        &current,
        &relative,
        staging.path(),
        &generated.assets,
        &before,
        &after,
    )?;
    Ok(ModelImportResult {
        model: format!("{prefix}:model"),
        prefab: format!("{prefix}:prefab"),
        directory: relative,
        assets: generated.assets,
        fingerprint: blake3::Hash::from(scene.fingerprint.0).to_hex().to_string(),
        manifest_revision: blake3::hash(&after).to_hex().to_string(),
        warnings: generated.warnings,
    })
}
