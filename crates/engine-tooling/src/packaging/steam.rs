//! Steam variant packaging: staging Valve's `steam_api` redistributable beside a
//! packaged runtime, and the SteamPipe depot layout of `--format steam-depot`.
//!
//! A runtime built with the real Steam backend (tooling built with `--features
//! steam`) links the library dynamically and cannot start without it, so every
//! package format places it where the loader finds it ([`SteamLibrary`]). The file
//! is copied byte for byte and never modified: Valve's redistribution terms forbid it
//! and, on macOS, Valve's own code signature must stay valid.
//!
//! Depot layout (one platform per invocation, the host's):
//!
//! ```text
//! <destination>/content/<windows|linux>/   the directory package plus the library
//! <destination>/content/macos/<id>.app     the universal, signed app bundle
//! <destination>/scripts/app_build_<app id>.vdf
//! <destination>/scripts/depot_build_<depot id>.vdf
//! <destination>/README.txt
//! ```
//!
//! The VDF files are templates for `steamcmd +run_app_build`; no credential is ever
//! written. `steam_appid.txt` is never produced.

use engine_project::{Error, Project, Result};
use engine_steam::redistributable::{self, LibraryFile};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Names the library file to stage; wins over every other source.
pub const LIBRARY_ENV: &str = "ENGINE_STEAM_API_LIB";
/// The Steamworks SDK directory; the library is read from
/// `redistributable_bin/<platform>` below it.
pub const SDK_ENV: &str = redistributable::SDK_LOCATION_ENV;

/// True when this tooling build links the real Steam backend (`--features steam`).
pub const BACKEND_COMPILED: bool = cfg!(feature = "steam");

/// The platforms a depot can hold, named like the `content/<platform>` directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepotPlatform {
    Windows,
    Linux,
    Macos,
}

impl DepotPlatform {
    /// The platform of `host`, or `None` for one SteamPipe packaging does not cover.
    pub fn of(host: &engine_platform::PackageHost) -> Option<Self> {
        match host.platform {
            engine_platform::HostPlatform::Windows => Some(Self::Windows),
            engine_platform::HostPlatform::Linux => Some(Self::Linux),
            engine_platform::HostPlatform::Macos => Some(Self::Macos),
            _ => None,
        }
    }

    /// Directory name below `content/`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

/// Where a located library came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibrarySource {
    /// `ENGINE_STEAM_API_LIB`.
    Explicit,
    /// `$STEAM_SDK_LOCATION/redistributable_bin/<platform>`.
    Sdk,
    /// The pinned `steamworks-sys` source in the Cargo registry.
    SteamworksSys,
}

impl LibrarySource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Explicit => "ENGINE_STEAM_API_LIB",
            Self::Sdk => "STEAM_SDK_LOCATION",
            Self::SteamworksSys => "steamworks-sys",
        }
    }
}

/// Valve's redistributable library for the platform being packaged.
#[derive(Clone, Debug)]
pub struct SteamLibrary {
    /// The file to copy.
    pub path: PathBuf,
    /// The name the loader asks for (`steam_api64.dll`, `libsteam_api.so`,
    /// `libsteam_api.dylib`); the copy is made under this name.
    pub file: &'static str,
    pub source: LibrarySource,
}

impl SteamLibrary {
    /// Locate the library for the process's OS and architecture from the process
    /// environment (see [`Self::locate`]).
    pub fn from_process() -> Result<Self> {
        let cargo_home = redistributable::cargo_home(
            std::env::var_os("CARGO_HOME").as_deref(),
            std::env::var_os("USERPROFILE").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        Self::locate(
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::env::var_os(LIBRARY_ENV).as_deref(),
            std::env::var_os(SDK_ENV).as_deref(),
            cargo_home.as_deref(),
        )
    }

    /// The lookup with every input injected, in this order:
    ///
    /// 1. `explicit` (`ENGINE_STEAM_API_LIB`), a file named like the platform's
    ///    library; used or refused, never skipped.
    /// 2. `sdk` (`STEAM_SDK_LOCATION`): `redistributable_bin/<platform>/<file>`;
    ///    skipped when the file is not there, as the `engine-steam` build script does.
    /// 3. The highest `steamworks-sys-*` source under `cargo_home`.
    ///
    /// - `tooling.steam.platform_unsupported`: Valve ships no library for `os`/`arch`.
    /// - `tooling.steam.library_invalid`: `explicit` is not a file with the right name.
    /// - `tooling.steam.library_missing`: nothing found.
    pub fn locate(
        os: &str,
        arch: &str,
        explicit: Option<&OsStr>,
        sdk: Option<&OsStr>,
        cargo_home: Option<&Path>,
    ) -> Result<Self> {
        let library = redistributable::library_file(os, arch).ok_or_else(|| {
            Error::new(
                "tooling.steam.platform_unsupported",
                format!("Valve ships no steam_api library for {os} on {arch}"),
            )
        })?;
        if let Some(value) = explicit.filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            let invalid = |reason: String| Error::new("tooling.steam.library_invalid", reason);
            if !path.is_file() {
                return Err(invalid(format!("{LIBRARY_ENV} does not name a file")));
            }
            if path.file_name() != Some(OsStr::new(library.file)) {
                return Err(invalid(format!(
                    "{LIBRARY_ENV} must name a file called {} for {os} on {arch}",
                    library.file
                )));
            }
            return Ok(Self::found(path, library, LibrarySource::Explicit));
        }
        if let Some(sdk) = sdk.filter(|value| !value.is_empty())
            && let Some(path) = redistributable::in_sdk(Path::new(sdk), library)
        {
            return Ok(Self::found(path, library, LibrarySource::Sdk));
        }
        if let Some(home) = cargo_home
            && let Some(path) = redistributable::in_cargo_registry(home, library)
        {
            return Ok(Self::found(path, library, LibrarySource::SteamworksSys));
        }
        Err(Error::new(
            "tooling.steam.library_missing",
            format!(
                "{} was not found. Set {LIBRARY_ENV} to the file, or {SDK_ENV} to a Steamworks SDK \
                 holding redistributable_bin/{}/{0}, or build once with the steam feature so the \
                 steamworks-sys source is in the Cargo registry",
                library.file, library.directory
            ),
        ))
    }

    fn found(path: PathBuf, library: LibraryFile, source: LibrarySource) -> Self {
        Self {
            path,
            file: library.file,
            source,
        }
    }

    /// Copy the library byte for byte into `directory` under its loader name, leaving
    /// it writable by the owner so staging directories can be removed.
    pub fn copy_into(&self, directory: &Path) -> Result<PathBuf> {
        let target = directory.join(self.file);
        fs::copy(&self.path, &target)?;
        let mut permissions = fs::metadata(&target)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o644);
        }
        #[cfg(not(unix))]
        {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
        }
        fs::set_permissions(&target, permissions)?;
        Ok(target)
    }
}

/// The library a package of `project` needs: located when this tooling build has the
/// real Steam backend and the manifest has a `steam` block, otherwise `None` (a
/// default build, or a project without Steam, is packaged exactly as before).
pub fn library_for_package(project: &Project) -> Result<Option<SteamLibrary>> {
    if BACKEND_COMPILED && project.steam.is_some() {
        SteamLibrary::from_process().map(Some)
    } else {
        Ok(None)
    }
}

/// With the real Steam backend compiled in, a package must carry a `steam` block:
/// the runtime links the backend whatever the manifest says, and a package without
/// the matching library could not start. Refused with `tooling.steam.manifest_missing`
/// (`backend_compiled` is [`BACKEND_COMPILED`] in production; tests inject it). Call
/// before compiling. A tooling build without the backend never refuses.
pub fn require_manifest(project: &Project, backend_compiled: bool) -> Result<()> {
    if backend_compiled && project.steam.is_none() {
        return Err(Error::new(
            "tooling.steam.manifest_missing",
            "This tooling is built with the Steam backend, so its packages need a steam block with app_id in the project manifest (otherwise the package could not start); package with tooling built without --features steam, or add the block",
        ));
    }
    Ok(())
}

/// What a depot build needs from the manifest and the options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepotPlan {
    pub app_id: u32,
    pub depot_id: u32,
}

impl DepotPlan {
    /// - `tooling.steam_depot.manifest_missing`: the manifest has no `steam` block.
    /// - `tooling.steam_depot.depot_id_invalid`: `depot_id` is zero or the app id.
    pub fn new(project: &Project, depot_id: Option<u32>) -> Result<Self> {
        let app_id = project
            .steam
            .as_ref()
            .ok_or_else(|| {
                Error::new(
                    "tooling.steam_depot.manifest_missing",
                    "--format steam-depot needs a steam block with app_id in the project manifest",
                )
            })?
            .app_id;
        let depot_id = depot_id.unwrap_or_else(|| default_depot_id(app_id));
        if depot_id == 0 || depot_id == app_id {
            return Err(Error::new(
                "tooling.steam_depot.depot_id_invalid",
                format!("Depot id {depot_id} must be a positive number other than the app id"),
            ));
        }
        Ok(Self { app_id, depot_id })
    }
}

/// The depot id used when `--depot-id` is not given: the app id plus one.
///
/// This is the engine's convention, not a Steamworks guarantee: Valve's uploading
/// guide (https://partner.steamgames.com/doc/sdk/uploading) uses depot 1001 for app
/// 1000 in its examples, but states no rule, and depot ids are assigned in the
/// partner site. Pass `--depot-id` with the real id (always, for a second or third
/// platform depot).
pub fn default_depot_id(app_id: u32) -> u32 {
    app_id.saturating_add(1)
}

/// Refuse a depot build that cannot succeed, before anything is compiled: the
/// manifest, the Steam backend and the library (`tooling.steam_depot.backend_missing`
/// when the tooling was built without `--features steam`).
pub fn check_depot(project: &Project, depot_id: Option<u32>) -> Result<(DepotPlan, SteamLibrary)> {
    let plan = DepotPlan::new(project, depot_id)?;
    if !BACKEND_COMPILED {
        return Err(Error::new(
            "tooling.steam_depot.backend_missing",
            "--format steam-depot needs tooling built with the real Steam backend: cargo build -p engine-tooling --features steam --bin engine",
        ));
    }
    Ok((plan, SteamLibrary::from_process()?))
}

/// A string as a VDF (KeyValues) quoted token: backslash, quote, line feed and tab
/// are escaped, other control characters become spaces.
pub fn vdf_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => quoted.push(' '),
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

fn vdf_entry(indent: usize, key: &str, value: &str) -> String {
    format!(
        "{}{} {}\n",
        "\t".repeat(indent),
        vdf_quote(key),
        vdf_quote(value)
    )
}

/// File name of the app build script.
pub fn app_build_file(app_id: u32) -> String {
    format!("app_build_{app_id}.vdf")
}

/// File name of the depot build script.
pub fn depot_build_file(depot_id: u32) -> String {
    format!("depot_build_{depot_id}.vdf")
}

/// `scripts/app_build_<app id>.vdf`: the app build template. `ContentRoot` and
/// `BuildOutput` are relative to the script's directory (Valve's uploading guide);
/// forward slashes are used so nothing needs escaping. `SetLive` is empty, so the
/// build is uploaded but no branch is changed.
pub fn app_build_vdf(plan: DepotPlan) -> String {
    let mut vdf =
        String::from("// SteamPipe app build template; edit Desc, then see ../README.txt.\n");
    vdf.push_str("\"AppBuild\"\n{\n");
    vdf.push_str(&vdf_entry(1, "AppID", &plan.app_id.to_string()));
    vdf.push_str(&vdf_entry(1, "Desc", "TODO: describe this build"));
    vdf.push_str(&vdf_entry(1, "Preview", "0"));
    vdf.push_str(&vdf_entry(1, "SetLive", ""));
    vdf.push_str(&vdf_entry(1, "ContentRoot", "../content/"));
    vdf.push_str(&vdf_entry(1, "BuildOutput", "../output/"));
    vdf.push_str("\t\"Depots\"\n\t{\n");
    vdf.push_str(&vdf_entry(
        2,
        &plan.depot_id.to_string(),
        &depot_build_file(plan.depot_id),
    ));
    vdf.push_str("\t}\n}\n");
    vdf
}

/// `scripts/depot_build_<depot id>.vdf`: maps `content/<platform>/` to the depot
/// root and leaves debug files out.
pub fn depot_build_vdf(plan: DepotPlan, platform: DepotPlatform) -> String {
    let mut vdf = String::from("// SteamPipe depot build template for one platform.\n");
    vdf.push_str("\"DepotBuildConfig\"\n{\n");
    vdf.push_str(&vdf_entry(1, "DepotID", &plan.depot_id.to_string()));
    vdf.push_str("\t\"FileMapping\"\n\t{\n");
    vdf.push_str(&vdf_entry(
        2,
        "LocalPath",
        &format!("{}/*", platform.name()),
    ));
    vdf.push_str(&vdf_entry(2, "DepotPath", "."));
    vdf.push_str(&vdf_entry(2, "Recursive", "1"));
    vdf.push_str("\t}\n");
    for pattern in ["*.pdb", "*.debug", "*.dSYM"] {
        vdf.push_str(&vdf_entry(1, "FileExclusion", pattern));
    }
    vdf.push_str("}\n");
    vdf
}

/// The `README.txt` of a depot: layout, upload and combining depots.
pub fn readme(plan: DepotPlan, platform: DepotPlatform, launch: &str) -> String {
    let app = app_build_file(plan.app_id);
    let depot = depot_build_file(plan.depot_id);
    let platform_name = platform.name();
    let (app_id, depot_id) = (plan.app_id, plan.depot_id);
    let (second, third) = (depot_id.saturating_add(1), depot_id.saturating_add(2));
    format!(
        r#"SteamPipe depot for app {app_id}, platform {platform_name} (depot {depot_id}).

Layout
  content/{platform_name}/   the game for this platform, with Valve's steam_api library
  scripts/{app}   app build template (edit Desc; SetLive is empty, so no branch changes)
  scripts/{depot}   depot build template (maps content/{platform_name}/ to the depot root)

Before uploading
  * Depot id {depot_id} is the engine's default (app id + 1), not something Steamworks
    guarantees. Check it against the depot you created in the Steamworks partner site and
    repackage with --depot-id <n> if it differs. Restrict the depot to {platform_name} there.
  * Set the launch option in Steamworks to {launch}.
  * steam_appid.txt is deliberately absent: Steam supplies the app id when it launches the
    game. Do not add it.

Upload with SteamCMD (the Steamworks SDK ContentBuilder includes it)
  steamcmd +login <account> +run_app_build "<absolute path>/scripts/{app}" +quit
  SteamCMD prompts for the password and Steam Guard code; none is stored in these files. Use a
  Steamworks account that may publish builds. Build logs and cache go to output/ (created on
  the first run).

Several platforms
  Build on each host with its own depot id (--depot-id {depot_id}, {second} and {third}, for
  example). Then merge into one directory: copy every content/<platform>/ folder into one
  content/, every scripts/depot_build_<id>.vdf into one scripts/, and add one line
  "<depot id>" "depot_build_<depot id>.vdf" inside "Depots" of one app build script (delete the
  other hosts' copies of it). Move the files as archives that keep Unix permissions and, for
  the .app, symbolic links and signatures (tar, or ditto on macOS): a plain copy through a file
  system without them breaks the runtime's execute bit and the bundle's code signature.
"#
    )
}

/// Write `scripts/` and `README.txt` into the depot root `root`.
pub fn write_depot_files(
    root: &Path,
    plan: DepotPlan,
    platform: DepotPlatform,
    launch: &str,
) -> Result<()> {
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts)?;
    fs::write(
        scripts.join(app_build_file(plan.app_id)),
        app_build_vdf(plan),
    )?;
    fs::write(
        scripts.join(depot_build_file(plan.depot_id)),
        depot_build_vdf(plan, platform),
    )?;
    fs::write(root.join("README.txt"), readme(plan, platform, launch))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> DepotPlan {
        DepotPlan {
            app_id: 480,
            depot_id: 481,
        }
    }

    #[test]
    fn quoting_escapes_what_vdf_requires() {
        assert_eq!(vdf_quote("plain"), "\"plain\"");
        assert_eq!(vdf_quote(""), "\"\"");
        assert_eq!(vdf_quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(vdf_quote("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(vdf_quote("a\u{1}b"), "\"a b\"");
    }

    #[test]
    fn app_build_template_has_the_documented_keys() {
        let vdf = app_build_vdf(plan());
        assert_eq!(
            vdf,
            "// SteamPipe app build template; edit Desc, then see ../README.txt.\n\
\"AppBuild\"\n{\n\
\t\"AppID\" \"480\"\n\
\t\"Desc\" \"TODO: describe this build\"\n\
\t\"Preview\" \"0\"\n\
\t\"SetLive\" \"\"\n\
\t\"ContentRoot\" \"../content/\"\n\
\t\"BuildOutput\" \"../output/\"\n\
\t\"Depots\"\n\t{\n\
\t\t\"481\" \"depot_build_481.vdf\"\n\
\t}\n}\n"
        );
    }

    #[test]
    fn depot_build_template_maps_the_platform_directory() {
        for (platform, local) in [
            (DepotPlatform::Windows, "windows/*"),
            (DepotPlatform::Linux, "linux/*"),
            (DepotPlatform::Macos, "macos/*"),
        ] {
            let vdf = depot_build_vdf(plan(), platform);
            assert!(vdf.contains("\"DepotBuildConfig\"\n{\n"), "{vdf}");
            assert!(vdf.contains("\t\"DepotID\" \"481\"\n"), "{vdf}");
            assert!(
                vdf.contains(&format!("\t\t\"LocalPath\" \"{local}\"\n")),
                "{vdf}"
            );
            assert!(vdf.contains("\t\t\"DepotPath\" \".\"\n"), "{vdf}");
            assert!(vdf.contains("\t\t\"Recursive\" \"1\"\n"), "{vdf}");
            assert!(vdf.contains("\t\"FileExclusion\" \"*.pdb\"\n"), "{vdf}");
            assert!(!vdf.contains('\\'), "{vdf}");
        }
    }

    #[test]
    fn a_steam_backend_needs_the_manifest_block() {
        let write = |steam: &str| {
            let temp = tempfile::tempdir().unwrap();
            let manifest = temp.path().join("project.json");
            fs::write(
                &manifest,
                format!(
                    r#"{{"schema":1,"name":"g","startup_scene":"m.scene.json","assets":[]{steam}}}"#
                ),
            )
            .unwrap();
            fs::write(
                temp.path().join("m.scene.json"),
                r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
            )
            .unwrap();
            (Project::load(&manifest).unwrap(), temp)
        };
        let (without, _a) = write("");
        let (with, _b) = write(r#","steam":{"app_id":480}"#);
        let error = require_manifest(&without, true).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "tooling.steam.manifest_missing");
        require_manifest(&without, false).unwrap();
        require_manifest(&with, true).unwrap();
        require_manifest(&with, false).unwrap();
    }

    #[test]
    fn readme_keeps_its_layout_and_never_holds_credentials() {
        let text = readme(plan(), DepotPlatform::Linux, "runtime");
        assert!(
            text.contains(
                "
  content/linux/   the game"
            ),
            "indentation is kept:
{text}"
        );
        assert!(text.contains("+login <account> +run_app_build"), "{text}");
        assert!(text.contains("--depot-id 481, 482 and 483"), "{text}");
        assert!(
            text.contains("launch option in Steamworks to runtime."),
            "{text}"
        );
        assert!(!text.to_lowercase().contains("password:"), "{text}");
    }

    #[test]
    fn depot_id_defaults_to_app_id_plus_one_and_is_validated() {
        let project = |steam: &str| {
            let temp = tempfile::tempdir().unwrap();
            let manifest = temp.path().join("project.json");
            fs::write(
                &manifest,
                format!(
                    r#"{{"schema":1,"name":"g","startup_scene":"m.scene.json","assets":[]{steam}}}"#
                ),
            )
            .unwrap();
            fs::write(
                temp.path().join("m.scene.json"),
                r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
            )
            .unwrap();
            let loaded = Project::load(&manifest).unwrap();
            (temp, loaded)
        };
        let (_keep, with) = project(r#","steam":{"app_id":480}"#);
        assert_eq!(DepotPlan::new(&with, None).unwrap(), plan());
        assert_eq!(DepotPlan::new(&with, Some(7)).unwrap().depot_id, 7);
        for bad in [0, 480] {
            let error = DepotPlan::new(&with, Some(bad)).unwrap_err();
            assert_eq!(
                error.diagnostics[0].code,
                "tooling.steam_depot.depot_id_invalid"
            );
        }
        let (_keep, without) = project("");
        let error = DepotPlan::new(&without, None).unwrap_err();
        assert_eq!(
            error.diagnostics[0].code,
            "tooling.steam_depot.manifest_missing"
        );
        assert_eq!(default_depot_id(u32::MAX), u32::MAX);
    }

    #[test]
    fn library_lookup_order_and_errors() {
        let temp = tempfile::tempdir().unwrap();
        let code = |result: Result<SteamLibrary>| result.unwrap_err().diagnostics[0].code.clone();
        // Registry copy (lowest priority).
        let home = temp.path().join("home");
        let registry = home
            .join("registry/src/idx/steamworks-sys-0.13.0/lib/steam/redistributable_bin/linux64");
        fs::create_dir_all(&registry).unwrap();
        fs::write(registry.join("libsteam_api.so"), "registry").unwrap();
        // SDK copy.
        let sdk = temp.path().join("sdk");
        fs::create_dir_all(sdk.join("redistributable_bin/linux64")).unwrap();
        fs::write(
            sdk.join("redistributable_bin/linux64/libsteam_api.so"),
            "sdk",
        )
        .unwrap();
        // Explicit copy.
        let explicit = temp.path().join("custom/libsteam_api.so");
        fs::create_dir_all(explicit.parent().unwrap()).unwrap();
        fs::write(&explicit, "explicit").unwrap();
        let locate = |explicit: Option<&Path>, sdk: Option<&Path>, home: Option<&Path>| {
            SteamLibrary::locate(
                "linux",
                "x86_64",
                explicit.map(Path::as_os_str),
                sdk.map(Path::as_os_str),
                home,
            )
        };
        let read = |library: &SteamLibrary| fs::read_to_string(&library.path).unwrap();
        let all = locate(Some(&explicit), Some(&sdk), Some(&home)).unwrap();
        assert_eq!(
            (read(&all).as_str(), all.source),
            ("explicit", LibrarySource::Explicit)
        );
        let sdk_first = locate(None, Some(&sdk), Some(&home)).unwrap();
        assert_eq!(
            (read(&sdk_first).as_str(), sdk_first.source),
            ("sdk", LibrarySource::Sdk)
        );
        let registry_only = locate(None, None, Some(&home)).unwrap();
        assert_eq!(
            (read(&registry_only).as_str(), registry_only.source),
            ("registry", LibrarySource::SteamworksSys)
        );
        // An SDK directory without the file falls through to the registry.
        let empty_sdk = temp.path().join("empty-sdk");
        fs::create_dir(&empty_sdk).unwrap();
        assert_eq!(
            locate(None, Some(&empty_sdk), Some(&home)).unwrap().source,
            LibrarySource::SteamworksSys
        );
        // Errors.
        assert_eq!(
            code(locate(None, None, None)),
            "tooling.steam.library_missing"
        );
        assert_eq!(
            code(locate(None, None, Some(&temp.path().join("nowhere")))),
            "tooling.steam.library_missing"
        );
        assert_eq!(
            code(locate(
                Some(&temp.path().join("missing.so")),
                None,
                Some(&home)
            )),
            "tooling.steam.library_invalid"
        );
        let misnamed = temp.path().join("custom/steam.so");
        fs::write(&misnamed, "x").unwrap();
        assert_eq!(
            code(locate(Some(&misnamed), None, Some(&home))),
            "tooling.steam.library_invalid"
        );
        assert_eq!(
            code(SteamLibrary::locate("freebsd", "x86_64", None, None, None)),
            "tooling.steam.platform_unsupported"
        );
    }

    #[test]
    fn copy_keeps_bytes_and_leaves_the_copy_writable() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("steam_api64.dll");
        fs::write(&source, [0u8, 159, 146, 150, 255]).unwrap();
        let mut permissions = fs::metadata(&source).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let library = SteamLibrary {
            path: source,
            file: "steam_api64.dll",
            source: LibrarySource::Explicit,
        };
        let directory = temp.path().join("out");
        fs::create_dir(&directory).unwrap();
        let target = library.copy_into(&directory).unwrap();
        assert_eq!(fs::read(&target).unwrap(), [0u8, 159, 146, 150, 255]);
        assert!(!fs::metadata(&target).unwrap().permissions().readonly());
    }
}
