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
mod atomic;
mod limits;

pub(crate) use atomic::atomic_replace;
use atomic::reject_link;
pub use atomic::{FailureInjection, PersistenceStage};
use limits::{depth, parser_nesting_within, safe_name};
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
/// Called after a successful [`PersistenceStore::save`] with the slot name. It runs
/// after the slot lock is released, on the thread that saved.
pub type SaveListener = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
/// Version header of a stored snapshot (its data is not parsed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotHeader {
    pub schema: u32,
    pub content_version: u32,
}
pub struct PersistenceStore {
    options: PersistenceOptions,
    listener: std::sync::Mutex<Option<SaveListener>>,
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
        Ok(Self {
            options,
            listener: std::sync::Mutex::new(None),
        })
    }
    /// Registers (or clears) the post-save listener used to mirror saves elsewhere.
    pub fn set_save_listener(&self, listener: Option<SaveListener>) {
        *self.listener.lock().unwrap_or_else(|p| p.into_inner()) = listener;
    }
    fn notify_saved(&self, slot: &str) {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(listener) = listener {
            listener(slot);
        }
    }
    /// The validated, link-checked path of a save slot file (it may not exist yet).
    pub fn slot_path(&self, slot: &str) -> Result<PathBuf> {
        self.path("saves", slot)
    }
    /// Names of the existing save slots, sorted.
    pub fn slots(&self) -> Result<Vec<String>> {
        let probe = self.path("saves", "probe")?;
        let Some(dir) = probe.parent() else {
            return Ok(Vec::new());
        };
        let mut slots = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".json")
                && safe_name(stem)
            {
                slots.push(stem.to_owned());
            }
        }
        slots.sort();
        Ok(slots)
    }
    /// The version header of a slot, or `None` when the slot does not exist.
    pub fn slot_header(&self, slot: &str) -> Result<Option<SlotHeader>> {
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        reject_link(&path)?;
        if !path.exists() {
            return Ok(None);
        }
        self.parse_header(&read_bounded(&path, self.options.max_bytes)?)
            .map(Some)
    }
    /// Validates snapshot bytes (size, nesting, version header) and returns the header.
    pub fn parse_header(&self, bytes: &[u8]) -> Result<SlotHeader> {
        if bytes.len() as u64 > self.options.max_bytes {
            return Err(Error::new(
                "persistence.size.exceeded",
                "Snapshot exceeds byte limit",
            ));
        }
        if !parser_nesting_within(bytes) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Snapshot exceeds parser nesting safety limit",
            ));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::new("persistence.corrupt", e.to_string()))?;
        if !depth(&value, 0, self.options.max_depth) {
            return Err(Error::new(
                "persistence.depth.exceeded",
                "Snapshot JSON nesting exceeds limit",
            ));
        }
        let field = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .filter(|v| *v > 0 && *v <= u32::MAX as u64)
                .map(|v| v as u32)
                .ok_or_else(|| Error::new("persistence.corrupt", format!("Invalid {name}")))
        };
        let header = SlotHeader {
            schema: field("schema")?,
            content_version: field("content_version")?,
        };
        if header.schema > self.options.schema_version
            || header.content_version > self.options.content_version
        {
            return Err(Error::new(
                "persistence.version.newer",
                "Snapshot was written by a newer schema/content version",
            ));
        }
        serde_json::from_value::<Envelope>(value)
            .map_err(|e| Error::new("persistence.corrupt", e.to_string()))?;
        Ok(header)
    }
    /// Installs already-serialised snapshot bytes (for example from a cloud copy) as
    /// a slot, keeping the previous file as the backup. The bytes are validated like
    /// a read; invalid or newer-version bytes leave the slot untouched. No migration
    /// runs: the stored versions are preserved.
    pub fn install_slot_bytes(&self, slot: &str, bytes: &[u8]) -> Result<()> {
        self.parse_header(bytes)?;
        let path = self.path("saves", slot)?;
        let _lock = Self::lock(&path)?;
        if path.exists() {
            let previous = read_bounded(&path, self.options.max_bytes)?;
            atomic_replace(&path.with_extension("json.bak"), &previous, None)?;
        }
        atomic_replace(&path, bytes, self.options.fail)
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
        let root = user_data_base(UserDataOs::host(), |name| std::env::var_os(name));
        root.filter(|r| r.is_absolute())
            .map(|r| r.join(USER_DATA_FOLDER))
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
        let absolute = atomic::resolve_root(&self.options.root)?;
        let mut prefix = PathBuf::new();
        for component in absolute.components() {
            prefix.push(component);
            // A Windows drive prefix (including a verbatim canonical prefix)
            // is not a filesystem root until RootDir follows. Query the full
            // root and all descendants; already-rooted UNC/device prefixes
            // keep their metadata checks as well.
            if matches!(component, std::path::Component::Prefix(value)
                if matches!(value.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
            {
                continue;
            }
            reject_link(&prefix)?;
        }
        fs::create_dir_all(&absolute)?;
        let mut directory = fs::canonicalize(&absolute)?;
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
        {
            let _lock = Self::lock(&path)?;
            self.write(&path, data, migration)?;
        }
        self.notify_saved(slot);
        Ok(())
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

/// Folder under the platform data directory that holds all player data.
const USER_DATA_FOLDER: &str = "CPPGameEngine";

/// Operating-system family that selects the user-data base directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserDataOs {
    Windows,
    MacOs,
    Other,
}

impl UserDataOs {
    fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// Pure base-directory choice: `LOCALAPPDATA` on Windows, `~/Library/Application
/// Support` on macOS, `XDG_DATA_HOME` then `~/.local/share` elsewhere. `env` looks
/// up an environment variable; the caller applies the absolute-path check.
pub(crate) fn user_data_base(
    os: UserDataOs,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let home = || env("HOME").map(PathBuf::from);
    match os {
        UserDataOs::Windows => env("LOCALAPPDATA").map(PathBuf::from),
        UserDataOs::MacOs => home().map(|h| h.join("Library").join("Application Support")),
        UserDataOs::Other => env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(
        pairs: &[(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        let pairs = pairs.to_vec();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| std::ffi::OsString::from(*value))
        }
    }

    #[test]
    fn user_data_base_per_os() {
        let env = env_of(&[
            ("LOCALAPPDATA", "/local"),
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(
            user_data_base(UserDataOs::Windows, &env),
            Some(PathBuf::from("/local"))
        );
        assert_eq!(
            user_data_base(UserDataOs::MacOs, &env),
            Some(PathBuf::from("/home/u/Library/Application Support"))
        );
        assert_eq!(
            user_data_base(UserDataOs::Other, &env),
            Some(PathBuf::from("/xdg"))
        );
        let no_xdg = env_of(&[("HOME", "/home/u")]);
        assert_eq!(
            user_data_base(UserDataOs::Other, &no_xdg),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        // macOS ignores XDG_DATA_HOME; Windows ignores HOME.
        assert_eq!(
            user_data_base(UserDataOs::MacOs, env_of(&[("XDG_DATA_HOME", "/xdg")])),
            None
        );
        assert_eq!(
            user_data_base(UserDataOs::Windows, env_of(&[("HOME", "/home/u")])),
            None
        );
    }

    #[test]
    fn host_os_selects_matching_branch() {
        let (expected, want) = if cfg!(windows) {
            (UserDataOs::Windows, "/l")
        } else if cfg!(target_os = "macos") {
            (UserDataOs::MacOs, "/h/Library/Application Support")
        } else {
            (UserDataOs::Other, "/x")
        };
        assert_eq!(UserDataOs::host(), expected);
        let env = env_of(&[
            ("LOCALAPPDATA", "/l"),
            ("XDG_DATA_HOME", "/x"),
            ("HOME", "/h"),
        ]);
        assert_eq!(
            user_data_base(UserDataOs::host(), env),
            Some(PathBuf::from(want))
        );
    }

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
