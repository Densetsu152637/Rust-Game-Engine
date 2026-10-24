use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
};

pub(super) fn import_lock(root: &Path) -> Result<File> {
    let path = root.join(".model-import.lock");
    if path.symlink_metadata().is_ok_and(|m| is_link(&m)) {
        return Err(Error::new(
            "asset.import.path",
            "Import lock cannot be a link",
        ));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| Error::new("asset.import.busy", "Another model import is active"))?;
    Ok(file)
}
fn unchanged(path: &Path, expected: &[u8]) -> Result<()> {
    if read_bounded(path, 16 * 1024 * 1024)? != expected {
        return Err(Error::new(
            "asset.import.conflict",
            "Manifest changed during import; generated assets were not published",
        ));
    }
    Ok(())
}
pub(super) fn publish(
    project: &Project,
    relative: &Path,
    staging: &Path,
    assets: &[Asset],
    before: &[u8],
    after: &[u8],
) -> Result<()> {
    unchanged(&project.manifest, before)?;
    let parent = project.root.join("assets");
    let parent_created = match fs::create_dir(&parent) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e.into()),
    };
    if is_link(&parent.symlink_metadata()?)
        || !parent.is_dir()
        || !parent.canonicalize()?.starts_with(&project.root)
    {
        return Err(Error::new(
            "asset.import.path",
            "Asset output directory must be a real directory inside the project",
        ));
    }
    let destination = project.root.join(relative);
    if let Err(e) = fs::create_dir(&destination) {
        if parent_created {
            let _ = fs::remove_dir(&parent);
        }
        return Err(Error::new(
            "asset.import.output_exists",
            format!("Cannot create new output directory: {e}"),
        ));
    }
    let mut published = Vec::new();
    let result = (|| {
        for asset in assets {
            let name = asset
                .path
                .file_name()
                .ok_or_else(|| Error::new("asset.import.path", "Generated file has no name"))?;
            let target = destination.join(name);
            let mut input = File::open(staging.join(name))?;
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)?;
            published.push(target);
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        let mut candidate = project.clone();
        for asset in assets {
            candidate.assets.insert(asset.id.clone(), asset.clone());
        }
        candidate.validate()?;
        let mut temporary = tempfile::NamedTempFile::new_in(&project.root)?;
        temporary.write_all(after)?;
        temporary.as_file().sync_all()?;
        unchanged(&project.manifest, before)?;
        // Preserve original permissions across the atomic manifest replacement.
        temporary
            .as_file()
            .set_permissions(fs::metadata(&project.manifest)?.permissions())?;
        temporary
            .persist(&project.manifest)
            .map_err(|e| Error::from(e.error))?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut cleanup_errors = Vec::new();
        for path in published.iter().rev() {
            if let Err(e) = fs::remove_file(path) {
                cleanup_errors.push(e.to_string());
            }
        }
        if let Err(e) = fs::remove_dir(&destination) {
            cleanup_errors.push(e.to_string());
        }
        if parent_created && let Err(e) = fs::remove_dir(&parent) {
            cleanup_errors.push(e.to_string());
        }
        if !cleanup_errors.is_empty() {
            return Err(Error::new(
                "asset.import.cleanup",
                format!(
                    "{error}; generated-file cleanup failed: {}",
                    cleanup_errors.join("; ")
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
/// Streaming serialization stops before a generated file can exceed its cap.
pub(super) fn json_file(path: &Path, value: &impl serde::Serialize, maximum: usize) -> Result<()> {
    struct LimitedWriter {
        file: File,
        remaining: usize,
    }
    impl Write for LimitedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(std::io::Error::other(
                    "Generated JSON exceeds its byte limit",
                ));
            }
            let count = self.file.write(bytes)?;
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    let mut writer = LimitedWriter {
        file: File::create(path)?,
        remaining: maximum,
    };
    serde_json::to_writer(&mut writer, value)?;
    writer.flush()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optimistic_guard_rejects_changed_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("project.json");
        fs::write(&path, b"new").unwrap();
        assert!(unchanged(&path, b"old").is_err());
        assert_eq!(fs::read(path).unwrap(), b"new");
    }
    #[test]
    fn failed_candidate_validation_removes_generated_files_and_preserves_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let manifest = root.join("project.json");
        fs::write(
            &manifest,
            br#"{"schema":1,"name":"rollback","startup_scene":"main.json","assets":[]}"#,
        )
        .unwrap();
        fs::write(
            root.join("main.json"),
            br#"{"schema":1,"scene_id":"main","entities":[]}"#,
        )
        .unwrap();
        let project = Project::load(&manifest).unwrap();
        let staging = tempfile::tempdir_in(root).unwrap();
        fs::write(staging.path().join("bad.json"), b"invalid mesh").unwrap();
        let assets = vec![Asset {
            id: "bad".into(),
            path: "assets/imported-test/bad.json".into(),
            kind: "mesh".into(),
        }];
        let before = fs::read(&manifest).unwrap();
        assert!(
            publish(
                &project,
                Path::new("assets/imported-test"),
                staging.path(),
                &assets,
                &before,
                b"replacement"
            )
            .is_err()
        );
        assert_eq!(fs::read(&manifest).unwrap(), before);
        assert!(!root.join("assets").exists());
    }
    #[test]
    fn concurrent_import_lock_is_exclusive() {
        let directory = tempfile::tempdir().unwrap();
        let first = import_lock(directory.path()).unwrap();
        assert!(import_lock(directory.path()).is_err());
        drop(first);
        assert!(import_lock(directory.path()).is_ok());
    }
}
