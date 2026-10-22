use super::*;

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
