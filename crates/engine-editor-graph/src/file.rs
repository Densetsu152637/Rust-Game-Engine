use crate::GraphSession;
use engine_graph::{GraphDiagnostic, GraphDocument, GraphKind, MAX_DOCUMENT_BYTES};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Debug)]
pub enum FileError {
    Io(std::io::Error),
    Path,
    Conflict,
    ReadOnly,
    TooLarge,
    Diagnostics(Vec<GraphDiagnostic>),
}
impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Graph file: {e}"),
            Self::Path => f.write_str("Expected a .json file inside the project"),
            Self::Conflict => f.write_str("Graph changed on disk; reopen before saving"),
            Self::ReadOnly => f.write_str("Graph is read only"),
            Self::TooLarge => f.write_str("Graph exceeds 4 MiB"),
            Self::Diagnostics(d) => match d.first() {
                Some(first) => write!(
                    f,
                    "{} {}: {} ({} diagnostics)",
                    first.code,
                    first.path,
                    first.message,
                    d.len()
                ),
                None => f.write_str("Graph validation failed"),
            },
        }
    }
}
impl std::error::Error for FileError {}
impl From<std::io::Error> for FileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// One project-confined source file with an optimistic byte revision.
/// Reopening is explicit: saving never silently replaces external changes.
pub struct GraphFile {
    root: PathBuf,
    relative: PathBuf,
    baseline: Option<Vec<u8>>,
}
impl GraphFile {
    /// Open a project graph using the same builtin registry as project validation and MCP.
    pub fn open_registered(
        root: &Path,
        relative: &Path,
    ) -> Result<(Self, GraphSession), FileError> {
        let root = fs::canonicalize(root)?;
        let path = resolve(&root, relative)?;
        let bytes = read(&path)?;
        let document =
            GraphDocument::from_json(&bytes).map_err(|d| FileError::Diagnostics(vec![d]))?;
        let kind = engine_project::graphs::kinds()
            .get(&document.kind)
            .ok_or_else(|| {
                FileError::Diagnostics(vec![GraphDiagnostic::new(
                    "graph.kind.unknown",
                    "/kind",
                    format!("Unknown graph kind {}", document.kind),
                )])
            })?;
        let session = GraphSession::new(document, kind.clone()).map_err(FileError::Diagnostics)?;
        Ok((
            Self {
                root,
                relative: relative.to_owned(),
                baseline: Some(bytes),
            },
            session,
        ))
    }
    pub fn open(
        root: &Path,
        relative: &Path,
        kind: &GraphKind,
    ) -> Result<(Self, GraphSession), FileError> {
        let root = fs::canonicalize(root)?;
        let path = resolve(&root, relative)?;
        let bytes = read(&path)?;
        let document =
            GraphDocument::from_json(&bytes).map_err(|d| FileError::Diagnostics(vec![d]))?;
        let session = GraphSession::new(document, kind.clone()).map_err(FileError::Diagnostics)?;
        Ok((
            Self {
                root,
                relative: relative.to_owned(),
                baseline: Some(bytes),
            },
            session,
        ))
    }
    pub fn create(
        root: &Path,
        relative: &Path,
        kind: &GraphKind,
    ) -> Result<(Self, GraphSession), FileError> {
        let root = fs::canonicalize(root)?;
        let path = resolve(&root, relative)?;
        if path.exists() {
            return Err(FileError::Conflict);
        }
        let session = GraphSession::new(GraphDocument::new(&kind.name), kind.clone())
            .map_err(FileError::Diagnostics)?;
        Ok((
            Self {
                root,
                relative: relative.to_owned(),
                baseline: None,
            },
            session,
        ))
    }
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    /// Full validation is required before replacing an active graph asset.
    pub fn save(&mut self, session: &mut GraphSession) -> Result<(), FileError> {
        if session.is_read_only() {
            return Err(FileError::ReadOnly);
        }
        let diagnostics = session.diagnostics();
        if !diagnostics.is_empty() {
            return Err(FileError::Diagnostics(diagnostics));
        }
        let bytes = session.document().to_json_pretty().into_bytes();
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(FileError::TooLarge);
        }
        let path = resolve(&self.root, &self.relative)?;
        let original = fs::metadata(&path).ok();
        if original
            .as_ref()
            .is_some_and(|m| m.permissions().readonly())
        {
            return Err(FileError::ReadOnly);
        }
        self.check(&path)?;
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or(FileError::Path)?)?;
        if let Some(original) = original {
            temporary
                .as_file()
                .set_permissions(original.permissions())?;
        }
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        // Re-resolve and compare immediately before publish, matching graph_edit.
        if resolve(&self.root, &self.relative)? != path {
            return Err(FileError::Conflict);
        }
        self.check(&path)?;
        if self.baseline.is_some() {
            temporary
                .persist(&path)
                .map_err(|e| FileError::Io(e.error))?;
        } else {
            temporary
                .persist_noclobber(&path)
                .map_err(|e| FileError::Io(e.error))?;
        }
        self.baseline = Some(bytes);
        session.mark_saved();
        Ok(())
    }
    /// Export work in progress to a new, separate file. The host must not register it as an active asset.
    pub fn export_draft(&self, relative: &Path, session: &GraphSession) -> Result<(), FileError> {
        let path = resolve(&self.root, relative)?;
        if path == resolve(&self.root, &self.relative)? || path.exists() {
            return Err(FileError::Conflict);
        }
        let bytes = session.document().to_json_pretty().into_bytes();
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(FileError::TooLarge);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or(FileError::Path)?)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        if resolve(&self.root, relative)? != path {
            return Err(FileError::Conflict);
        }
        temporary
            .persist_noclobber(&path)
            .map_err(|e| FileError::Io(e.error))?;
        Ok(())
    }
    fn check(&self, path: &Path) -> Result<(), FileError> {
        match &self.baseline {
            Some(expected) if read(path)? != *expected => Err(FileError::Conflict),
            None if path.exists() => Err(FileError::Conflict),
            _ => Ok(()),
        }
    }
}
fn resolve(root: &Path, relative: &Path) -> Result<PathBuf, FileError> {
    if relative.as_os_str().is_empty()
        || relative.extension().and_then(|s| s.to_str()) != Some("json")
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(FileError::Path);
    }
    let joined = root.join(relative);
    let parent = fs::canonicalize(joined.parent().ok_or(FileError::Path)?)?;
    if !parent.starts_with(root) {
        return Err(FileError::Path);
    }
    if joined.exists() {
        return engine_project::confined_file(root, relative).map_err(|_| FileError::Path);
    }
    Ok(parent.join(joined.file_name().ok_or(FileError::Path)?))
}
fn read(path: &Path) -> Result<Vec<u8>, FileError> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(FileError::TooLarge);
    }
    Ok(bytes)
}
