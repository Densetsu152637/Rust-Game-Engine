//! Confined source/asset exports. Generated output is separate from authoring data.
use crate::compiler::BuildArtifact;
use engine_project::{Error, Project, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetRecord {
    pub id: String,
    pub path: PathBuf,
    pub kind: String,
    pub content_hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetIndex {
    pub schema: u32,
    pub assets: Vec<AssetRecord>,
}

pub fn asset_index(project: &Project) -> Result<AssetIndex> {
    let assets = project
        .assets
        .values()
        .map(|asset| {
            let path = project.resolve_asset(&asset.id)?;
            Ok(AssetRecord {
                id: asset.id.clone(),
                path: asset.path.clone(),
                kind: asset.kind.clone(),
                content_hash: format!("{:x}", Sha256::digest(fs::read(path)?)),
            })
        })
        .collect::<Result<_>>()?;
    Ok(AssetIndex { schema: 1, assets })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShaderRecord {
    pub id: String,
    pub hash: String,
    pub path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShaderImport {
    pub directory: PathBuf,
    pub shaders: Vec<ShaderRecord>,
}

/// WGSL is the native wgpu source format. Import validates without opening a GPU
/// and writes content-hashed sources plus `index.json` only after every source
/// passes, so an invalid shader leaves the previous cache index untouched.
pub fn import_shaders(project: &Project) -> Result<ShaderImport> {
    let directory = confined_output_directory(&project.root, Path::new(".derived/shaders"))?;
    let mut imported = Vec::new();
    for asset in project
        .assets
        .values()
        .filter(|asset| asset.kind == "shader")
    {
        let path = project.resolve_asset(&asset.id)?;
        let source = fs::read_to_string(path)?;
        if source.len() > 8 * 1024 * 1024 {
            return Err(Error::new("shader.too_large", "WGSL source exceeds 8 MiB"));
        }
        engine_desktop::validate_wgsl(&source)
            .map_err(|e| Error::new("shader.invalid", e.to_string()).at(&asset.path, ""))?;
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        imported.push((asset.id.clone(), hash, source));
    }
    for (_, hash, source) in &imported {
        fs::write(directory.join(format!("{hash}.wgsl")), source)?;
    }
    let shaders = imported
        .into_iter()
        .map(|(id, hash, _)| ShaderRecord {
            id,
            path: format!("{hash}.wgsl"),
            hash,
        })
        .collect::<Vec<_>>();
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(&shaders)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(directory.join("index.json"))
        .map_err(|e| Error::from(e.error))?;
    Ok(ShaderImport { directory, shaders })
}

/// Atomic cache replacement under the canonical project root. A pre-existing
/// `.derived` link cannot redirect writes outside the opened project.
pub fn write_asset_index(project: &Project) -> Result<PathBuf> {
    let index = asset_index(project)?;
    let directory = confined_output_directory(&project.root, Path::new(".derived"))?;
    let destination = directory.join("assets.json");
    if destination.exists() && is_link(&destination.symlink_metadata()?) {
        return Err(Error::new(
            "tooling.cache.path",
            "Asset cache must not be a symlink",
        ));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(&index)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&destination)
        .map_err(|e| Error::from(e.error))?;
    Ok(destination)
}

pub(crate) fn confined_output_directory(root: &Path, relative: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let mut directory = root.clone();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(Error::new(
                "tooling.path",
                "Expected relative output directory",
            ));
        };
        directory.push(part);
        if directory.exists() {
            if is_link(&directory.symlink_metadata()?) {
                return Err(Error::new(
                    "tooling.path",
                    "Generated output cannot traverse a symlink",
                ));
            }
        } else {
            fs::create_dir(&directory)?;
        }
        if !directory.canonicalize()?.starts_with(&root) {
            return Err(Error::new(
                "tooling.path",
                "Generated output escapes project root",
            ));
        }
    }
    Ok(directory)
}

/// Copy a validated template into a new destination; no existing tree is changed.
pub fn initialize_project(template_manifest: &Path, destination: &Path) -> Result<PathBuf> {
    let template = Project::load(template_manifest)?;
    reject_source_destination(&template.root, destination, false)?;
    export_new_directory(destination, |staging| {
        copy_tree(&template.root, staging)?;
        let manifest = staging.join(
            template
                .manifest
                .file_name()
                .ok_or_else(|| Error::new("tooling.manifest", "Manifest has no file name"))?,
        );
        Project::load(&manifest)?;
        Ok(())
    })?;
    Ok(destination.join(
        template
            .manifest
            .file_name()
            .expect("validated manifest name"),
    ))
}

/// Package the already compiled native game as a directory. See
/// [`crate::packaging::package`] for the archive formats.
pub fn package_project(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
) -> Result<PathBuf> {
    crate::packaging::package(
        project,
        artifact,
        destination,
        engine_root,
        crate::packaging::PackageFormat::Dir,
    )
}

/// Checks a package destination without building: it must be new and must not
/// overlap authored source except under generated `build`/`dist`/`release`.
pub fn check_package_destination(project: &Project, destination: &Path) -> Result<()> {
    reject_source_destination(&project.root, destination, true)?;
    if destination.exists() {
        return Err(Error::new(
            "tooling.destination.exists",
            "Destination must be new",
        ));
    }
    Ok(())
}

/// Best-effort removal of older generation directories in a native cache. The
/// `current` generation, the `keep - 1` most recently modified others, and any
/// generation with a live readiness control file (a running game) are kept.
pub fn prune_generations(cache: &Path, current: &Path, keep: usize) -> Result<usize> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(cache)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir()
            || path == current
            || entry.file_name() == "target"
            || !path.join("generation.json").is_file()
        {
            continue;
        }
        let live = fs::read_dir(&path)?
            .filter_map(|item| item.ok())
            .any(|item| {
                item.path()
                    .extension()
                    .is_some_and(|extension| extension == "ready")
            });
        if !live {
            candidates.push((entry.metadata()?.modified()?, path));
        }
    }
    candidates.sort();
    let excess = candidates.len().saturating_sub(keep.saturating_sub(1));
    let mut removed = 0;
    for (_, path) in candidates.into_iter().take(excess) {
        // A generation still mapped by a process cannot be removed on Windows.
        if fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

pub(crate) fn reject_source_destination(
    source: &Path,
    destination: &Path,
    allow_derived: bool,
) -> Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let name = destination.file_name().ok_or_else(|| {
        Error::new(
            "tooling.destination.invalid",
            "Destination requires a directory name",
        )
    })?;
    let resolved = parent.join(name);
    if let Ok(relative) = resolved.strip_prefix(source) {
        let derived=relative.components().next().is_some_and(|c|matches!(c,Component::Normal(name) if ["build","dist","release",".derived"].iter().any(|allowed|name==*allowed)));
        if !allow_derived || !derived {
            return Err(Error::new(
                "tooling.destination.source",
                "Destination cannot overlap authored source; use a sibling or generated dist/build directory",
            ));
        }
    }
    Ok(())
}

pub(crate) fn write_dependency_notices(artifact: &BuildArtifact, destination: &Path) -> Result<()> {
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(artifact.directory.join("cargo-metadata.json"))?)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| Error::new("package.metadata", "Cargo metadata packages missing"))?;
    let mut notices = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap_or("unknown");
        let version = package["version"].as_str().unwrap_or("unknown");
        if name == "engine-runtime-generation" {
            continue;
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
        {
            return Err(Error::new(
                "package.metadata",
                "Invalid Cargo package identity",
            ));
        }
        let source = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or_else(|| Error::new("package.metadata", "Package manifest missing"))?,
        )
        .parent()
        .ok_or_else(|| Error::new("package.metadata", "Package root missing"))?;
        let mut copied = Vec::new();
        for file in fs::read_dir(source)? {
            let file = file?;
            let upper = file.file_name().to_string_lossy().to_uppercase();
            if file.file_type()?.is_file()
                && ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            {
                let folder = destination.join(format!("{name}-{version}"));
                fs::create_dir_all(&folder)?;
                fs::copy(file.path(), folder.join(file.file_name()))?;
                copied.push(file.file_name().to_string_lossy().into_owned());
            }
        }
        notices.push(serde_json::json!({"name":name,"version":version,"license":package["license"],"notices":copied,"local":package["source"].is_null()}));
    }
    fs::write(
        destination.join("dependencies.json"),
        serde_json::to_vec_pretty(&notices)?,
    )?;
    Ok(())
}

pub(crate) fn export_new_directory(
    destination: &Path,
    populate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if destination.exists() {
        return Err(Error::new(
            "tooling.destination.exists",
            "Destination must be new",
        ));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize()?;
    let staging = tempfile::Builder::new()
        .prefix(".engine-export-")
        .tempdir_in(parent)?;
    populate(staging.path())?;
    // A temporary directory is created 0700; the published one is not private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o755))?;
    }
    if destination.exists() {
        return Err(Error::new(
            "tooling.destination.exists",
            "Destination appeared during export",
        ));
    }
    fs::rename(staging.path(), destination)?;
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        if [
            ".git", "target", "build", "dist", "release", ".derived", "saves", "logs",
        ]
        .iter()
        .any(|skip| name == *skip)
        {
            continue;
        }
        if is_link(&entry.path().symlink_metadata()?) {
            return Err(Error::new(
                "tooling.path",
                "Exports do not follow source symlinks",
            ));
        }
        let output = destination.join(&name);
        if kind.is_dir() {
            fs::create_dir(&output)?;
            copy_tree(&entry.path(), &output)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), output)?;
        }
    }
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
