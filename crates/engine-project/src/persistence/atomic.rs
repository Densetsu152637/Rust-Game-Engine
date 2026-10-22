use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceStage {
    Write,
    Flush,
    Replace,
}
pub type FailureInjection = Option<fn(PersistenceStage) -> bool>;
pub(super) fn reject_link(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            let link = {
                use std::os::windows::fs::MetadataExt;
                meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let link = meta.file_type().is_symlink();
            if link {
                return Err(Error::new(
                    "persistence.path.link",
                    "Persistence paths cannot be symbolic links or junctions",
                )
                .at(path, ""));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
/// Temporary file is created in the destination directory, then flushed and atomically replaced.
pub(crate) fn atomic_replace(path: &Path, bytes: &[u8], fail: FailureInjection) -> Result<()> {
    reject_link(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("persistence.path.invalid", "No parent directory"))?;
    reject_link(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if fail.is_some_and(|f| f(PersistenceStage::Write)) {
        return Err(Error::new(
            "persistence.write.failed",
            "Injected write failure",
        ));
    }
    temporary.write_all(bytes)?;
    if fail.is_some_and(|f| f(PersistenceStage::Flush)) {
        return Err(Error::new(
            "persistence.flush.failed",
            "Injected flush failure",
        ));
    }
    temporary.as_file().sync_all()?;
    if fail.is_some_and(|f| f(PersistenceStage::Replace)) {
        return Err(Error::new(
            "persistence.replace.failed",
            "Injected replacement failure",
        ));
    }
    // Windows refuses to rename over a file while another thread briefly holds a
    // handle (for example a concurrent link check); retry before reporting failure.
    let mut attempt = 0_u64;
    loop {
        match temporary.persist(path) {
            Ok(_) => break,
            Err(e) if attempt < 8 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5 * attempt));
                temporary = e.file;
            }
            Err(e) => return Err(Error::new("persistence.replace.failed", e.to_string())),
        }
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
