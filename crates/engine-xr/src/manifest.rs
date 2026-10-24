//! Active OpenXR runtime manifest discovery and parsing.
//!
//! Resolution order (first existing file wins; an existing but invalid file is
//! reported, not skipped, matching the Khronos loader):
//! 1. `XR_RUNTIME_JSON` when set and non-empty — exclusive on every platform.
//! 2. Windows: registry `HKLM\SOFTWARE\Khronos\OpenXR\1` value `ActiveRuntime`.
//! 3. Linux: `$XDG_CONFIG_HOME/openxr/1/` (default `$HOME/.config/openxr/1/`),
//!    then each `$XDG_CONFIG_DIRS` entry (default `/etc/xdg`), then `/etc`;
//!    in each directory `active_runtime.<arch>.json` before `active_runtime.json`.
//! 4. macOS and other platforms: unsupported unless `XR_RUNTIME_JSON` is set.
//!
//! All inputs are captured in [`DiscoveryInputs`] so tests inject them instead
//! of mutating the process environment.
use crate::status::{CheckedLocation, ManifestSource, XrStatus};
use std::{
    ffi::OsString,
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
};

/// Environment override naming the runtime manifest to use.
pub const RUNTIME_JSON_ENV: &str = "XR_RUNTIME_JSON";
/// Registry location of the active runtime on Windows.
pub const WINDOWS_ACTIVE_RUNTIME: &str =
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime";
/// Upper bound on a manifest's size; larger files are rejected unread.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// Upper bound on `runtime.library_path`.
pub const MAX_LIBRARY_PATH_BYTES: usize = 4096;
/// Default negotiation entry point exported by runtimes.
pub const NEGOTIATE_SYMBOL: &str = "xrNegotiateLoaderRuntimeInterface";

/// Platform conventions that govern manifest discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
    Other,
}
impl Platform {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::MacOs => "macOS",
            Self::Other => "this platform",
        }
    }
}

/// Everything discovery reads from the machine, captured up front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryInputs {
    pub platform: Platform,
    /// `XR_RUNTIME_JSON` (empty values are treated as unset).
    pub runtime_json: Option<PathBuf>,
    /// Windows registry `ActiveRuntime` value, or why it could not be read.
    /// `None` when not queried (non-Windows).
    pub windows_active_runtime: Option<Result<PathBuf, String>>,
    pub xdg_config_home: Option<PathBuf>,
    /// Raw `XDG_CONFIG_DIRS` (colon separated).
    pub xdg_config_dirs: Option<OsString>,
    pub home: Option<PathBuf>,
    /// Architecture name used in `active_runtime.<arch>.json`.
    pub arch: &'static str,
}
impl DiscoveryInputs {
    /// Inputs with nothing set for `platform`.
    pub fn empty(platform: Platform) -> Self {
        Self {
            platform,
            runtime_json: None,
            windows_active_runtime: None,
            xdg_config_home: None,
            xdg_config_dirs: None,
            home: None,
            arch: std::env::consts::ARCH,
        }
    }
    /// Read the real environment (and, on Windows, the registry).
    pub fn from_environment() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let platform = Platform::current();
        Self {
            platform,
            runtime_json: var(RUNTIME_JSON_ENV).map(PathBuf::from),
            windows_active_runtime: windows_active_runtime(),
            xdg_config_home: var("XDG_CONFIG_HOME").map(PathBuf::from),
            xdg_config_dirs: var("XDG_CONFIG_DIRS"),
            home: var("HOME").map(PathBuf::from),
            arch: std::env::consts::ARCH,
        }
    }
}
#[cfg(windows)]
fn windows_active_runtime() -> Option<Result<PathBuf, String>> {
    Some(crate::registry::active_runtime())
}
#[cfg(not(windows))]
fn windows_active_runtime() -> Option<Result<PathBuf, String>> {
    None
}

/// A manifest path to examine, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestCandidate {
    pub source: ManifestSource,
    pub path: PathBuf,
}

/// Candidate manifest paths in resolution order, plus sources that could not
/// produce a path at all (for example an absent registry value).
pub fn candidates(inputs: &DiscoveryInputs) -> (Vec<ManifestCandidate>, Vec<CheckedLocation>) {
    let mut found = Vec::new();
    let mut checked = Vec::new();
    if let Some(path) = &inputs.runtime_json {
        found.push(ManifestCandidate {
            source: ManifestSource::EnvironmentOverride,
            path: path.clone(),
        });
        return (found, checked);
    }
    match inputs.platform {
        Platform::Windows => match &inputs.windows_active_runtime {
            Some(Ok(path)) => found.push(ManifestCandidate {
                source: ManifestSource::WindowsRegistry,
                path: path.clone(),
            }),
            Some(Err(reason)) => checked.push(CheckedLocation {
                source: ManifestSource::WindowsRegistry,
                location: WINDOWS_ACTIVE_RUNTIME.into(),
                outcome: reason.clone(),
            }),
            None => checked.push(CheckedLocation {
                source: ManifestSource::WindowsRegistry,
                location: WINDOWS_ACTIVE_RUNTIME.into(),
                outcome: "not queried".into(),
            }),
        },
        Platform::Linux => {
            let mut directories = Vec::new();
            let home_config = inputs
                .xdg_config_home
                .clone()
                .filter(|p| absolute(p))
                .or_else(|| inputs.home.as_ref().map(|home| home.join(".config")));
            match home_config {
                Some(dir) => directories.push((ManifestSource::XdgConfigHome, dir)),
                None => checked.push(CheckedLocation {
                    source: ManifestSource::XdgConfigHome,
                    location: "$XDG_CONFIG_HOME".into(),
                    outcome: "neither XDG_CONFIG_HOME nor HOME is set".into(),
                }),
            }
            let config_dirs: Vec<PathBuf> = inputs
                .xdg_config_dirs
                .as_ref()
                .map(|dirs| {
                    dirs.to_string_lossy()
                        .split(':')
                        .map(PathBuf::from)
                        .filter(|p| absolute(p))
                        .collect()
                })
                .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
                .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
            directories.extend(
                config_dirs
                    .into_iter()
                    .map(|dir| (ManifestSource::XdgConfigDirs, dir)),
            );
            directories.push((ManifestSource::SystemConfig, PathBuf::from("/etc")));
            for (source, dir) in directories {
                let base = dir.join("openxr").join("1");
                for name in [
                    format!("active_runtime.{}.json", inputs.arch),
                    "active_runtime.json".to_owned(),
                ] {
                    found.push(ManifestCandidate {
                        source,
                        path: base.join(name),
                    });
                }
            }
        }
        Platform::MacOs | Platform::Other => {}
    }
    (found, checked)
}

/// XDG directories must be absolute; Unix-style roots count on every host so
/// the Linux rules are testable anywhere.
fn absolute(path: &Path) -> bool {
    path.is_absolute() || path.to_string_lossy().starts_with('/')
}

/// How the runtime library named by a manifest is located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryLocation {
    /// Absolute path, or a relative path resolved against the manifest's directory.
    Path(PathBuf),
    /// A bare file name resolved by the operating system's library search.
    SystemSearch(String),
}
impl fmt::Display for LibraryLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::SystemSearch(name) => write!(f, "{name} (system library search)"),
        }
    }
}

/// A parsed, validated runtime manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeManifest {
    pub manifest_path: PathBuf,
    pub source: ManifestSource,
    /// `runtime.name`, when present.
    pub name: Option<String>,
    pub library: LibraryLocation,
    /// Negotiation entry point (`functions` may rename it).
    pub negotiate_symbol: String,
}
impl RuntimeManifest {
    /// The runtime name, else the library location.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| self.library.to_string())
    }
}

/// Why a manifest that exists cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    Io(String),
    Oversize { bytes: u64 },
    Malformed(String),
    LibraryMissing(PathBuf),
}
impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "cannot read: {reason}"),
            Self::Oversize { bytes } => write!(
                f,
                "manifest is {bytes} bytes, above the {MAX_MANIFEST_BYTES}-byte limit"
            ),
            Self::Malformed(reason) => write!(f, "malformed manifest: {reason}"),
            Self::LibraryMissing(path) => {
                write!(f, "runtime library {} does not exist", path.display())
            }
        }
    }
}

fn malformed(reason: impl Into<String>) -> ManifestError {
    ManifestError::Malformed(reason.into())
}
fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Parse manifest JSON. Relative library paths resolve against the manifest's
/// directory and must exist; bare file names use the system library search.
pub fn parse_manifest(
    bytes: &[u8],
    manifest_path: &Path,
    source: ManifestSource,
) -> Result<RuntimeManifest, ManifestError> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Oversize {
            bytes: bytes.len() as u64,
        });
    }
    let root: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    let root = root
        .as_object()
        .ok_or_else(|| malformed("top level must be an object"))?;
    let version = root
        .get("file_format_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| malformed("file_format_version must be a string"))?;
    if !version.starts_with("1.") {
        return Err(malformed(format!(
            "unsupported file_format_version {version}"
        )));
    }
    let runtime = root
        .get("runtime")
        .and_then(|v| v.as_object())
        .ok_or_else(|| malformed("runtime must be an object"))?;
    let library = runtime
        .get("library_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| malformed("runtime.library_path must be a string"))?;
    if library.is_empty() || library.len() > MAX_LIBRARY_PATH_BYTES || library.contains('\0') {
        return Err(malformed(format!(
            "runtime.library_path must be 1..={MAX_LIBRARY_PATH_BYTES} bytes without NUL"
        )));
    }
    let name = match runtime.get("name") {
        None => None,
        Some(serde_json::Value::String(name)) if name.len() <= 256 => Some(name.clone()),
        Some(_) => {
            return Err(malformed(
                "runtime.name must be a string of at most 256 bytes",
            ));
        }
    };
    let negotiate_symbol = match runtime
        .get("functions")
        .or_else(|| root.get("functions"))
        .and_then(|functions| functions.get(NEGOTIATE_SYMBOL))
    {
        None => NEGOTIATE_SYMBOL.to_owned(),
        Some(serde_json::Value::String(symbol)) if is_identifier(symbol) => symbol.clone(),
        Some(_) => {
            return Err(malformed(format!(
                "functions.{NEGOTIATE_SYMBOL} must be a C identifier"
            )));
        }
    };
    let library = resolve_library(library, manifest_path)?;
    Ok(RuntimeManifest {
        manifest_path: manifest_path.to_path_buf(),
        source,
        name,
        library,
        negotiate_symbol,
    })
}

fn resolve_library(library: &str, manifest_path: &Path) -> Result<LibraryLocation, ManifestError> {
    let path = Path::new(library);
    let components: Vec<_> = path.components().collect();
    if !path.is_absolute() && components.len() == 1 {
        return Ok(LibraryLocation::SystemSearch(library.to_owned()));
    }
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let base = manifest_path.parent().unwrap_or(Path::new("."));
        components
            .into_iter()
            .filter(|c| !matches!(c, Component::CurDir))
            .fold(base.to_path_buf(), |dir, c| dir.join(c))
    };
    if resolved.is_file() {
        Ok(LibraryLocation::Path(resolved))
    } else {
        Err(ManifestError::LibraryMissing(resolved))
    }
}

/// Read a candidate with a hard size bound and parse it.
pub fn read_manifest(candidate: &ManifestCandidate) -> Result<RuntimeManifest, ManifestError> {
    let file =
        std::fs::File::open(&candidate.path).map_err(|e| ManifestError::Io(e.to_string()))?;
    let length = file
        .metadata()
        .map_err(|e| ManifestError::Io(e.to_string()))?
        .len();
    if length > MAX_MANIFEST_BYTES {
        return Err(ManifestError::Oversize { bytes: length });
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ManifestError::Io(e.to_string()))?;
    parse_manifest(&bytes, &candidate.path, candidate.source)
}

/// Resolve the active runtime manifest, or the honest reason there is none.
pub fn resolve(inputs: &DiscoveryInputs) -> Result<RuntimeManifest, XrStatus> {
    if inputs.runtime_json.is_none() && matches!(inputs.platform, Platform::MacOs | Platform::Other)
    {
        return Err(XrStatus::PlatformUnsupported {
            platform: inputs.platform.name().into(),
        });
    }
    let (candidates, mut checked) = candidates(inputs);
    for candidate in candidates {
        if !candidate.path.is_file() {
            checked.push(CheckedLocation {
                source: candidate.source,
                location: candidate.path.display().to_string(),
                outcome: "file not found".into(),
            });
            continue;
        }
        return read_manifest(&candidate).map_err(|error| XrStatus::ManifestInvalid {
            manifest: candidate.path.clone(),
            reason: error.to_string(),
        });
    }
    Err(XrStatus::RuntimeNotFound { checked })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary directory")
    }
    fn write_manifest(dir: &Path, name: &str, library: &str) -> PathBuf {
        let path = dir.join(name);
        let body = serde_json::json!({
            "file_format_version": "1.0.0",
            "runtime": {"name": "Test Runtime", "library_path": library}
        });
        std::fs::write(&path, body.to_string()).unwrap();
        path
    }

    #[test]
    fn environment_override_is_exclusive() {
        let mut inputs = DiscoveryInputs::empty(Platform::Windows);
        inputs.windows_active_runtime = Some(Ok(PathBuf::from(r"C:\runtime.json")));
        inputs.runtime_json = Some(PathBuf::from("override.json"));
        let (found, checked) = candidates(&inputs);
        assert_eq!(
            found,
            vec![ManifestCandidate {
                source: ManifestSource::EnvironmentOverride,
                path: PathBuf::from("override.json")
            }]
        );
        assert!(checked.is_empty());
    }

    #[test]
    fn windows_uses_the_registry_value_and_reports_its_absence() {
        let mut inputs = DiscoveryInputs::empty(Platform::Windows);
        inputs.windows_active_runtime = Some(Ok(PathBuf::from(r"C:\Oculus\oculus_openxr_64.json")));
        let (found, _) = candidates(&inputs);
        assert_eq!(found[0].source, ManifestSource::WindowsRegistry);
        assert_eq!(found.len(), 1);

        inputs.windows_active_runtime = Some(Err("key or value not present".into()));
        let status = resolve(&inputs).unwrap_err();
        assert_eq!(
            status,
            XrStatus::RuntimeNotFound {
                checked: vec![CheckedLocation {
                    source: ManifestSource::WindowsRegistry,
                    location: WINDOWS_ACTIVE_RUNTIME.into(),
                    outcome: "key or value not present".into(),
                }]
            }
        );
        assert!(status.to_string().contains(WINDOWS_ACTIVE_RUNTIME));
    }

    #[test]
    fn linux_order_is_config_home_then_config_dirs_then_etc() {
        let mut inputs = DiscoveryInputs::empty(Platform::Linux);
        inputs.arch = "x86_64";
        inputs.home = Some(PathBuf::from("/home/u"));
        inputs.xdg_config_dirs = Some(OsString::from("/opt/xdg:relative:/etc/xdg2"));
        let (found, checked) = candidates(&inputs);
        let paths: Vec<_> = found
            .iter()
            .map(|c| c.path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            paths,
            [
                "/home/u/.config/openxr/1/active_runtime.x86_64.json",
                "/home/u/.config/openxr/1/active_runtime.json",
                "/opt/xdg/openxr/1/active_runtime.x86_64.json",
                "/opt/xdg/openxr/1/active_runtime.json",
                "/etc/xdg2/openxr/1/active_runtime.x86_64.json",
                "/etc/xdg2/openxr/1/active_runtime.json",
                "/etc/openxr/1/active_runtime.x86_64.json",
                "/etc/openxr/1/active_runtime.json",
            ]
        );
        assert_eq!(found[0].source, ManifestSource::XdgConfigHome);
        assert_eq!(found[2].source, ManifestSource::XdgConfigDirs);
        assert_eq!(found[7].source, ManifestSource::SystemConfig);
        assert!(checked.is_empty());

        inputs.xdg_config_home = Some(PathBuf::from("/xdg/home"));
        inputs.xdg_config_dirs = None;
        let (found, _) = candidates(&inputs);
        assert!(found[0].path.starts_with("/xdg/home/openxr/1"));
        assert!(found[2].path.starts_with("/etc/xdg/openxr/1"));
    }

    #[test]
    fn macos_without_override_is_unsupported() {
        let status = resolve(&DiscoveryInputs::empty(Platform::MacOs)).unwrap_err();
        assert_eq!(
            status,
            XrStatus::PlatformUnsupported {
                platform: "macOS".into()
            }
        );
        assert_eq!(status.state(), "platform_unsupported");
    }

    #[test]
    fn missing_files_produce_runtime_not_found_with_every_location() {
        let dir = temp();
        let mut inputs = DiscoveryInputs::empty(Platform::Linux);
        inputs.arch = "x86_64";
        inputs.xdg_config_home = Some(dir.path().join("config"));
        inputs.xdg_config_dirs = Some(dir.path().join("dirs").into_os_string());
        // /etc candidates may exist on a developer Linux machine; tests only
        // run the full resolver against a temporary override there.
        if cfg!(windows) {
            let XrStatus::RuntimeNotFound { checked } = resolve(&inputs).unwrap_err() else {
                panic!("expected runtime_not_found");
            };
            assert_eq!(checked.len(), 6);
            assert!(checked.iter().all(|c| c.outcome == "file not found"));
        }
        inputs.runtime_json = Some(dir.path().join("absent.json"));
        let XrStatus::RuntimeNotFound { checked } = resolve(&inputs).unwrap_err() else {
            panic!("expected runtime_not_found");
        };
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].source, ManifestSource::EnvironmentOverride);
    }

    #[test]
    fn valid_manifest_with_relative_library_resolves_beside_the_manifest() {
        let dir = temp();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin").join("runtime.dll"), b"").unwrap();
        let manifest = write_manifest(dir.path(), "runtime.json", "./bin/runtime.dll");
        let mut inputs = DiscoveryInputs::empty(Platform::Windows);
        inputs.windows_active_runtime = Some(Ok(manifest.clone()));
        let parsed = resolve(&inputs).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Test Runtime"));
        assert_eq!(parsed.source, ManifestSource::WindowsRegistry);
        assert_eq!(
            parsed.library,
            LibraryLocation::Path(dir.path().join("bin").join("runtime.dll"))
        );
        assert_eq!(parsed.negotiate_symbol, NEGOTIATE_SYMBOL);
        assert_eq!(parsed.display_name(), "Test Runtime");
    }

    #[test]
    fn absolute_and_bare_library_paths() {
        let dir = temp();
        let library = dir.path().join("abs.so");
        std::fs::write(&library, b"").unwrap();
        let manifest = dir.path().join("m.json");
        let body = |lib: &str| {
            serde_json::json!({"file_format_version":"1.0.0","runtime":{"library_path":lib}})
                .to_string()
        };
        let parsed = parse_manifest(
            body(library.to_str().unwrap()).as_bytes(),
            &manifest,
            ManifestSource::EnvironmentOverride,
        )
        .unwrap();
        assert_eq!(parsed.library, LibraryLocation::Path(library));
        assert_eq!(parsed.name, None);
        let bare = parse_manifest(
            body("libruntime.so").as_bytes(),
            &manifest,
            ManifestSource::SystemConfig,
        )
        .unwrap();
        assert_eq!(
            bare.library,
            LibraryLocation::SystemSearch("libruntime.so".into())
        );
    }

    #[test]
    fn missing_library_is_reported_as_invalid_manifest() {
        let dir = temp();
        let manifest = write_manifest(dir.path(), "runtime.json", "./missing/runtime.dll");
        let mut inputs = DiscoveryInputs::empty(Platform::Windows);
        inputs.windows_active_runtime = Some(Ok(manifest.clone()));
        let status = resolve(&inputs).unwrap_err();
        let XrStatus::ManifestInvalid {
            manifest: path,
            reason,
        } = &status
        else {
            panic!("expected manifest_invalid, got {status:?}");
        };
        assert_eq!(path, &manifest);
        assert!(reason.contains("does not exist"), "{reason}");
        assert!(reason.contains("runtime.dll"), "{reason}");
    }

    #[test]
    fn malformed_manifests_are_rejected_with_reasons() {
        let path = Path::new("m.json");
        let cases: [(&str, &str); 7] = [
            ("not json", "malformed manifest"),
            ("[]", "top level must be an object"),
            (
                r#"{"runtime":{"library_path":"a.dll"}}"#,
                "file_format_version",
            ),
            (
                r#"{"file_format_version":"2.0.0","runtime":{"library_path":"a.dll"}}"#,
                "unsupported file_format_version",
            ),
            (
                r#"{"file_format_version":"1.0.0"}"#,
                "runtime must be an object",
            ),
            (
                r#"{"file_format_version":"1.0.0","runtime":{"library_path":""}}"#,
                "library_path",
            ),
            (
                r#"{"file_format_version":"1.0.0","runtime":{"library_path":"a.dll","functions":{"xrNegotiateLoaderRuntimeInterface":"bad name!"}}}"#,
                "C identifier",
            ),
        ];
        for (body, expected) in cases {
            let error = parse_manifest(body.as_bytes(), path, ManifestSource::EnvironmentOverride)
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{body}: {error}");
        }
        let renamed = parse_manifest(
            br#"{"file_format_version":"1.0.0","runtime":{"library_path":"a.dll","functions":{"xrNegotiateLoaderRuntimeInterface":"CustomNegotiate"}}}"#,
            path,
            ManifestSource::EnvironmentOverride,
        )
        .unwrap();
        assert_eq!(renamed.negotiate_symbol, "CustomNegotiate");
    }

    #[test]
    fn oversize_manifest_is_rejected_before_parsing() {
        let dir = temp();
        let path = dir.path().join("huge.json");
        let padding = " ".repeat(MAX_MANIFEST_BYTES as usize + 1);
        std::fs::write(&path, format!("{{{padding}}}")).unwrap();
        let mut inputs = DiscoveryInputs::empty(Platform::Linux);
        inputs.runtime_json = Some(path.clone());
        let status = resolve(&inputs).unwrap_err();
        let XrStatus::ManifestInvalid { reason, .. } = &status else {
            panic!("expected manifest_invalid, got {status:?}");
        };
        assert_eq!(
            reason,
            &format!(
                "manifest is {} bytes, above the 65536-byte limit",
                MAX_MANIFEST_BYTES + 3
            )
        );
        assert!(matches!(
            parse_manifest(
                &vec![b' '; 70_000],
                &path,
                ManifestSource::EnvironmentOverride
            ),
            Err(ManifestError::Oversize { bytes: 70_000 })
        ));
    }
}
