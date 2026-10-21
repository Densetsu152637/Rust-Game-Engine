//! Bounded JSON envelopes and explicit backup recovery; writes replace durably.
use crate::{Error, Result, schema::read_bounded};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceStage {
    Write,
    Flush,
    Replace,
}
pub type FailureInjection = Option<fn(PersistenceStage) -> bool>;
pub type Migration = fn(u32, u32, Map<String, Value>) -> Result<Map<String, Value>>;
#[derive(Clone, Debug)]
pub struct PersistenceOptions {
    pub root: PathBuf,
    pub project_id: String,
    pub schema_version: u32,
    pub content_version: u32,
    pub max_bytes: u64,
    pub max_depth: usize,
    pub fail: FailureInjection,
}
impl PersistenceOptions {
    pub fn new(root: PathBuf, project_id: String) -> Self {
        Self {
            root,
            project_id,
            schema_version: 1,
            content_version: 1,
            max_bytes: 1024 * 1024,
            max_depth: 32,
            fail: None,
        }
    }
}
pub struct PersistenceStore {
    options: PersistenceOptions,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    #[serde(rename = "content_version")]
    content: u32,
    data: Map<String, Value>,
}
/// Retains the stable on-disk profile directory used by authored desktop projects.
pub fn project_id(name: &str) -> String {
    let mut hash = 14695981039346656037_u64;
    for byte in name.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("project-{hash}")
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn reject_link(path: &Path) -> Result<()> {
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
    temporary
        .persist(path)
        .map_err(|e| Error::new("persistence.replace.failed", e.to_string()))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
fn depth(value: &Value, current: usize, limit: usize) -> bool {
    if current > limit {
        return false;
    }
    match value {
        Value::Array(items) => items.iter().all(|v| depth(v, current + 1, limit)),
        Value::Object(items) => items.values().all(|v| depth(v, current + 1, limit)),
        _ => true,
    }
}
impl PersistenceStore {
    pub fn new(options: PersistenceOptions) -> Result<Self> {
        if !safe_name(&options.project_id)
            || options.schema_version == 0
            || options.content_version == 0
            || options.max_bytes == 0
            || options.max_bytes > 64 * 1024 * 1024
            || options.max_depth == 0
            || options.max_depth > 64
        {
            return Err(Error::new(
                "persistence.options.invalid",
                "Invalid persistence options",
            ));
        }
        Ok(Self { options })
    }
    pub fn options(&self) -> &PersistenceOptions {
        &self.options
    }
    pub fn default_user_data_root(project_id: &str) -> Result<PathBuf> {
        if !safe_name(project_id) {
            return Err(Error::new(
                "persistence.project.invalid",
                "Invalid project ID",
            ));
        }
        #[cfg(windows)]
        let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")));
        root.filter(|r| r.is_absolute())
            .map(|r| r.join("CPPGameEngine"))
            .ok_or_else(|| {
                Error::new(
                    "persistence.root.unavailable",
                    "User data root is unavailable",
                )
            })
    }
    fn path(&self, area: &str, slot: &str) -> Result<PathBuf> {
        if !safe_name(slot) {
            return Err(Error::new(
                "persistence.slot.invalid",
                "Slot must be a bounded safe identifier",
            ));
        }
        let absolute = std::path::absolute(&self.options.root)?;
        let mut prefix = PathBuf::new();
        for component in absolute.components() {
            prefix.push(component);
            reject_link(&prefix)?;
        }
        fs::create_dir_all(&self.options.root)?;
        let mut directory = fs::canonicalize(&self.options.root)?;
        for component in [&self.options.project_id, area] {
            directory.push(component);
            reject_link(&directory)?;
            fs::create_dir_all(&directory)?;
        }
        let path = directory.join(format!("{slot}.json"));
        reject_link(&path)?;
        reject_link(&path.with_extension("json.bak"))?;
        reject_link(&path.with_extension("json.lock"))?;
        Ok(path)
    }
    fn lock(path: &Path) -> Result<File> {
        let path = path.with_extension("json.lock");
        reject_link(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock_exclusive()
            .map_err(|e| Error::new("persistence.lock.busy", e.to_string()))?;
        Ok(file)
    }
    fn read(&self, path: &Path, migration: Option<Migration>) -> Result<Map<String, Value>> {
        reject_link(path)?;
        if !path.exists() {
            return Err(Error::new("persistence.missing", "No snapshot exists").at(path, ""));
        }
        let bytes = read_bounded(path, self.options.max_bytes)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::new("persistence.corrupt", e.to_string()))?;
        if !depth(&value, 0, self.options.max_depth) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Snapshot JSON nesting exceeds limit",
            ));
        }
        let schema = value
            .get("schema")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= u32::MAX as u64)
            .ok_or_else(|| Error::new("persistence.corrupt", "Invalid snapshot schema"))?;
        let content = value
            .get("content_version")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= u32::MAX as u64)
            .ok_or_else(|| Error::new("persistence.corrupt", "Invalid content version"))?;
        if schema > self.options.schema_version as u64
            || content > self.options.content_version as u64
        {
            return Err(Error::new(
                "persistence.version.newer",
                "Snapshot was written by a newer schema/content version",
            ));
        }
        let envelope: Envelope = serde_json::from_value(value)
            .map_err(|e| Error::new("persistence.corrupt", e.to_string()))?;
        if envelope.schema == self.options.schema_version
            && envelope.content == self.options.content_version
        {
            return Ok(envelope.data);
        }
        let migrate = migration.ok_or_else(|| {
            Error::new(
                "persistence.migration.required",
                "Snapshot needs an explicit migration",
            )
        })?;
        let data = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            migrate(envelope.schema, envelope.content, envelope.data)
        }))
        .map_err(|_| {
            Error::new(
                "persistence.migration.failed",
                "Migration callback panicked",
            )
        })??;
        if !depth(&Value::Object(data.clone()), 0, self.options.max_depth) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Migrated snapshot exceeds depth limit",
            ));
        }
        if serde_json::to_vec(&data)?.len() as u64 > self.options.max_bytes {
            return Err(Error::new(
                "persistence.migration.invalid",
                "Migrated snapshot exceeds byte limit",
            ));
        }
        Ok(data)
    }
    fn write(
        &self,
        path: &Path,
        data: Map<String, Value>,
        migration: Option<Migration>,
    ) -> Result<()> {
        let envelope = Envelope {
            schema: self.options.schema_version,
            content: self.options.content_version,
            data,
        };
        let value = serde_json::to_value(&envelope)?;
        if !depth(&value, 0, self.options.max_depth) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Snapshot JSON nesting exceeds limit",
            ));
        }
        let bytes = serde_json::to_vec(&envelope)?;
        if bytes.len() as u64 > self.options.max_bytes {
            return Err(Error::new(
                "persistence.size.exceeded",
                "Snapshot exceeds byte limit",
            ));
        }
        if path.exists() {
            self.read(path, migration)?;
            let previous = read_bounded(path, self.options.max_bytes)?;
            let backup = path.with_extension("json.bak");
            if backup.exists() {
                match self.read(&backup, None) {
                    Ok(_) => {}
                    Err(error)
                        if error
                            .diagnostics
                            .first()
                            .is_some_and(|d| d.code == "persistence.migration.required") => {}
                    Err(error) => return Err(error),
                }
            }
            atomic_replace(&path.with_extension("json.bak"), &previous, None)?;
        }
        atomic_replace(path, &bytes, self.options.fail)
    }
    pub fn load(&self, slot: &str, migration: Option<Migration>) -> Result<Map<String, Value>> {
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        self.read(&path, migration)
    }
    pub fn save(
        &self,
        slot: &str,
        data: Map<String, Value>,
        migration: Option<Migration>,
    ) -> Result<()> {
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        self.write(&path, data, migration)
    }
    pub fn load_backup(
        &self,
        slot: &str,
        migration: Option<Migration>,
    ) -> Result<Map<String, Value>> {
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        self.read(&path.with_extension("json.bak"), migration)
    }
    pub fn recover_backup(&self, slot: &str, migration: Option<Migration>) -> Result<()> {
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        let data = self.read(&path.with_extension("json.bak"), migration)?;
        let bytes = serde_json::to_vec(&Envelope {
            schema: self.options.schema_version,
            content: self.options.content_version,
            data,
        })?;
        atomic_replace(&path, &bytes, self.options.fail)
    }
    pub fn load_settings(&self, migration: Option<Migration>) -> Result<Map<String, Value>> {
        let path = self.path("settings", "preferences")?;
        let _lock = Self::lock(&path)?;
        self.read(&path, migration)
    }
    pub fn save_settings(
        &self,
        data: Map<String, Value>,
        migration: Option<Migration>,
    ) -> Result<()> {
        let path = self.path("settings", "preferences")?;
        let _lock = Self::lock(&path)?;
        self.write(&path, data, migration)
    }
}
