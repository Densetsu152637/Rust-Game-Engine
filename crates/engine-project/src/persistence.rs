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
// The configured payload limit is <=64; reject excessive nesting before recursive
// parsing, keeping parser safety refusals distinct from recoverable malformed JSON.
fn parser_nesting_within(bytes: &[u8]) -> bool {
    let (mut nesting, mut quoted, mut escaped) = (0_u32, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match *byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    nesting += 1;
                    if nesting > 96 {
                        return false;
                    }
                }
                b'}' | b']' => nesting = nesting.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
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
        if !parser_nesting_within(&bytes) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Snapshot exceeds parser nesting safety limit",
            ));
        }
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
        // Explicit recovery may replace a recognized corrupt or missing primary,
        // but must never erase newer/unrecognized data or conceal an I/O refusal.
        if let Err(error) = self.read(&path, migration)
            && !error.diagnostics.first().is_some_and(|d| {
                matches!(
                    d.code.as_str(),
                    "persistence.missing" | "persistence.corrupt"
                )
            })
        {
            return Err(error);
        }
        let data = self.read(&path.with_extension("json.bak"), migration)?;
        let envelope = Envelope {
            schema: self.options.schema_version,
            content: self.options.content_version,
            data,
        };
        let value = serde_json::to_value(&envelope)?;
        if !depth(&value, 0, self.options.max_depth) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Recovered snapshot exceeds depth limit",
            ));
        }
        let bytes = serde_json::to_vec(&envelope)?;
        if bytes.len() as u64 > self.options.max_bytes {
            return Err(Error::new(
                "persistence.size.exceeded",
                "Recovered snapshot exceeds byte limit",
            ));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn atomic_replace_creates_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_replace(&path, b"one", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert_eq!(temp_files(dir.path()), vec!["a.json"]);
    }

    #[test]
    fn atomic_replace_overwrites_an_existing_file_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_replace(&path, b"a much longer first body", None).unwrap();
        atomic_replace(&path, b"short", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn atomic_replace_accepts_empty_and_large_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_replace(&path, b"", None).unwrap();
        assert!(fs::read(&path).unwrap().is_empty());
        let big = vec![7u8; 3 * 1024 * 1024];
        atomic_replace(&path, &big, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), big);
    }

    fn interrupted(stage: PersistenceStage, code: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        atomic_replace(&path, b"previous", None).unwrap();
        let fail: FailureInjection = Some(match stage {
            PersistenceStage::Write => |s| s == PersistenceStage::Write,
            PersistenceStage::Flush => |s| s == PersistenceStage::Flush,
            PersistenceStage::Replace => |s| s == PersistenceStage::Replace,
        });
        let error = atomic_replace(&path, b"next", fail).unwrap_err();
        assert_eq!(error.diagnostics[0].code, code);
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(temp_files(dir.path()), vec!["a.json"]);
    }

    #[test]
    fn interrupted_write_keeps_the_target_and_removes_the_temporary() {
        interrupted(PersistenceStage::Write, "persistence.write.failed");
    }

    #[test]
    fn interrupted_flush_keeps_the_target_and_removes_the_temporary() {
        interrupted(PersistenceStage::Flush, "persistence.flush.failed");
    }

    #[test]
    fn interrupted_replace_keeps_the_target_and_removes_the_temporary() {
        interrupted(PersistenceStage::Replace, "persistence.replace.failed");
    }

    #[test]
    fn atomic_replace_fails_when_the_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(atomic_replace(&dir.path().join("none/a.json"), b"x", None).is_err());
    }

    #[test]
    fn atomic_replace_refuses_to_replace_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.json");
        fs::create_dir(&target).unwrap();
        assert!(atomic_replace(&target, b"x", None).is_err());
        assert!(target.is_dir());
    }

    #[test]
    fn safe_names_are_ascii_identifiers_up_to_64_bytes() {
        assert!(safe_name("Slot_1-b"));
        assert!(safe_name(&"a".repeat(64)));
        assert!(!safe_name(&"a".repeat(65)));
        assert!(!safe_name(""));
        assert!(!safe_name("a.b"));
        assert!(!safe_name("\u{e9}"));
    }

    #[test]
    fn depth_counts_arrays_and_objects_and_checks_scalars_too() {
        let v = serde_json::json!({"a": [1, {"b": 2}]});
        assert!(depth(&v, 0, 3));
        assert!(!depth(&v, 0, 2));
        assert!(depth(&serde_json::json!(1), 0, 0));
        assert!(!depth(&serde_json::json!(1), 1, 0));
    }

    #[test]
    fn parser_nesting_guard_allows_96_levels_and_refuses_97() {
        let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(parser_nesting_within(nested(96).as_bytes()));
        assert!(!parser_nesting_within(nested(97).as_bytes()));
    }

    #[test]
    fn parser_nesting_guard_ignores_brackets_inside_strings_and_escapes() {
        assert!(parser_nesting_within(
            format!("\"{}\"", "[".repeat(500)).as_bytes()
        ));
        assert!(parser_nesting_within(br#"{"s":"\"[[[[[[[[[[[[["}"#));
        assert!(parser_nesting_within(b"]]]]]]"));
    }
}
