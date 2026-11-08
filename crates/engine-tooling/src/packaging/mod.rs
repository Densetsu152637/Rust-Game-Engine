//! Desktop packaging: one staged directory layout, exported in several formats.
//!
//! [`stage`] is the single source of the package layout. Directory output renames
//! the staged tree into place; archive formats ([`archive`]) are built from the
//! same staged tree, so a new format only adds a writer.
//!
//! The destination is always the path of the thing created: the new directory for
//! [`PackageFormat::Dir`], the archive file for the archive formats. It must not
//! exist. Archives hold one top-level folder named after the destination file
//! name without its format suffix (`dist/game-1.0.tar.gz` holds `game-1.0/`).
//! [`PackageFormat::AppImage`] writes one executable file: the staged tree plus
//! the AppImage files ([`appimage`]) is the AppDir that `appimagetool` packs.
//! [`PackageFormat::App`] arranges the same content as a macOS application bundle
//! ([`macos`]): `stage` chooses the layout by format, so what is copied stays in
//! one place.
//!
//! A project with a `steam` manifest block packaged by tooling built with the real
//! Steam backend also gets Valve's `steam_api` library beside the runtime
//! ([`steam::SteamLibrary`]); [`PackageFormat::SteamDepot`] arranges the package as
//! a SteamPipe depot ([`steam`]).

pub mod appimage;
pub mod archive;
pub mod macos;
pub mod steam;

use crate::artifacts::{copy_tree, export_new_directory, reject_source_destination};
use crate::compiler::BuildArtifact;
use engine_project::{Error, Project, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Output format of `engine project package`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackageFormat {
    /// A directory (default).
    #[default]
    Dir,
    /// A zip archive (deflate) with one top-level folder.
    Zip,
    /// A gzip-compressed tar archive with one top-level folder.
    TarGz,
    /// A Linux AppImage file, built with `appimagetool` on a Linux host.
    AppImage,
    /// A macOS `<Name>.app` bundle with a universal runtime, built on a macOS host.
    App,
    /// A SteamPipe depot directory (`content/<platform>/`, `scripts/`, `README.txt`)
    /// for the host platform, from Steam-backend tooling and a `steam` manifest block.
    SteamDepot,
}

impl PackageFormat {
    /// Names accepted by `--format`, for usage messages.
    pub const NAMES: &'static str = "dir|zip|tar.gz|appimage|app|steam-depot";

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dir" => Some(Self::Dir),
            "zip" => Some(Self::Zip),
            "tar.gz" => Some(Self::TarGz),
            "appimage" => Some(Self::AppImage),
            "app" => Some(Self::App),
            "steam-depot" => Some(Self::SteamDepot),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
            Self::AppImage => "appimage",
            Self::App => "app",
            Self::SteamDepot => "steam-depot",
        }
    }

    /// File name suffix of the archive, `None` for a directory.
    fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Dir | Self::AppImage | Self::App | Self::SteamDepot => None,
            Self::Zip => Some(".zip"),
            Self::TarGz => Some(".tar.gz"),
        }
    }
}

/// Package the already compiled native game in `format` at `destination`.
/// Launchers require neither Cargo nor source checkout paths. Gameplay sources
/// remain because authored script assets are stable content references, while
/// native execution uses the copied binary. For a Steam-variant package the
/// `steam_api` library is located from the process environment first
/// ([`steam::library_for_package`]).
pub fn package(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    format: PackageFormat,
) -> Result<PathBuf> {
    let library = steam::library_for_package(project)?;
    package_with_library(
        project,
        artifact,
        destination,
        engine_root,
        format,
        library.as_ref(),
    )
}

/// As [`package`] with the Steam library already resolved (`None` stages none).
/// [`PackageFormat::App`] and [`PackageFormat::SteamDepot`] are refused
/// (`package.format.app`, `package.format.steam_depot`): they need
/// [`package_app`] and [`package_steam_depot`].
pub fn package_with_library(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    format: PackageFormat,
    library: Option<&steam::SteamLibrary>,
) -> Result<PathBuf> {
    if format == PackageFormat::App {
        return Err(Error::new(
            "package.format.app",
            "--format app needs one build per architecture; use package_app",
        ));
    }
    if format == PackageFormat::SteamDepot {
        return Err(Error::new(
            "package.format.steam_depot",
            "--format steam-depot writes a depot layout; use package_steam_depot",
        ));
    }
    if format == PackageFormat::AppImage {
        // The host and tool are checked before anything is staged.
        let tooling = appimage::AppImageTooling::from_process()?;
        return package_appimage_with_library(
            project,
            artifact,
            destination,
            engine_root,
            &tooling,
            library,
        );
    }
    let package_host = engine_platform::PackageHost::current()
        .map_err(|error| Error::new("package.platform.unsupported", error.to_string()))?;
    let engine_license = validate_inputs(project, artifact, destination, engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format,
        steam_library: library,
    };
    match format {
        PackageFormat::Dir => export_new_directory(destination, |staging| stage(&inputs, staging))
            .map(|()| destination.to_path_buf()),
        PackageFormat::AppImage | PackageFormat::App | PackageFormat::SteamDepot => {
            unreachable!("returned above")
        }
        PackageFormat::Zip | PackageFormat::TarGz => {
            let folder = archive_folder(destination, format)?;
            // Relative to the package root.
            let executables = [package_host.runtime_filename.as_str(), "run.sh"];
            archive::export_new_archive(destination, &folder, format, &executables, |staging| {
                stage(&inputs, staging)
            })
            .map(|()| destination.to_path_buf())
        }
    }
}

/// Fail early when `format` cannot be produced on this host: AppImage needs a
/// Linux host and `appimagetool` (see [`appimage::AppImageTooling`]); an app bundle
/// needs a macOS host with `lipo` and `codesign` (see [`macos::MacosTooling`]).
/// Other formats need nothing.
pub fn check_format_available(format: PackageFormat) -> Result<()> {
    match format {
        PackageFormat::AppImage => {
            appimage::AppImageTooling::from_process()?;
        }
        PackageFormat::App => {
            macos::MacosTooling::from_process()?;
        }
        PackageFormat::SteamDepot if cfg!(target_os = "macos") => {
            macos::MacosTooling::from_process()?;
        }
        PackageFormat::Dir
        | PackageFormat::Zip
        | PackageFormat::TarGz
        | PackageFormat::SteamDepot => {}
    }
    Ok(())
}

/// Refuse a destination name the format cannot use, before anything is compiled:
/// an app bundle must end in `.app` (`tooling.destination.invalid`).
pub fn check_destination_name(format: PackageFormat, destination: &Path) -> Result<()> {
    if format == PackageFormat::App {
        macos::bundle_file_stem(destination)?;
    }
    Ok(())
}

/// The two single-architecture generations an app bundle joins into one runtime.
pub struct AppBuilds<'a> {
    /// Built for `aarch64-apple-darwin`; its generation is recorded in the bundle.
    pub arm64: &'a BuildArtifact,
    /// Built for `x86_64-apple-darwin`.
    pub x86_64: &'a BuildArtifact,
}

/// Package a macOS `.app` bundle at `destination` (a new directory ending in
/// `.app`) with the host and tools already resolved: stage the bundle, join the
/// two runtimes with `lipo`, sign it and publish it with a no-clobber rename. A
/// Steam-variant project also gets Valve's library in `Contents/Frameworks`
/// ([`steam::library_for_package`]).
pub fn package_app(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
) -> Result<PathBuf> {
    let library = steam::library_for_package(project)?;
    package_app_with_library(
        project,
        builds,
        destination,
        engine_root,
        bundle_id,
        tooling,
        library.as_ref(),
    )
}

/// As [`package_app`] with the Steam library already resolved.
pub fn package_app_with_library(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
    library: Option<&steam::SteamLibrary>,
) -> Result<PathBuf> {
    macos::bundle_file_stem(destination)?;
    macos::bundle_identifier(&project.name, bundle_id)?;
    check_app_builds(project, builds, destination, engine_root)?;
    macos::export_new_app(destination, |bundle| {
        build_app(
            project,
            builds,
            bundle,
            engine_root,
            bundle_id,
            tooling,
            library,
        )
    })
    .map(|()| destination.to_path_buf())
}

fn check_app_builds(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
) -> Result<()> {
    if !builds.x86_64.executable.is_file() {
        return Err(Error::new(
            "package.runtime.missing",
            "Compiled x86_64 generation executable is missing",
        ));
    }
    validate_inputs(project, builds.arm64, destination, engine_root).map(|_| ())
}

/// Everything that goes into the empty directory `bundle` (named `*.app`): the
/// staged package, the universal runtime, the Steam library when there is one,
/// then the signature. The Steam library is signed by Valve and is never signed
/// again (see [`macos::sign_bundle`]).
fn build_app(
    project: &Project,
    builds: &AppBuilds<'_>,
    bundle: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
    library: Option<&steam::SteamLibrary>,
) -> Result<()> {
    stage_bundle_with_library(
        project,
        builds.arm64,
        bundle,
        engine_root,
        bundle_id,
        library,
    )?;
    macos::make_universal(
        tooling,
        &builds.arm64.executable,
        &builds.x86_64.executable,
        bundle,
    )?;
    if let Some(library) = library {
        macos::point_runtime_at_frameworks(tooling, bundle, library)?;
    }
    macos::strip_extended_attributes(tooling, bundle)?;
    macos::sign_bundle(tooling, bundle)?;
    if library.is_some() {
        macos::verify_bundle(tooling, bundle)?;
    }
    Ok(())
}

/// The pure part of an app bundle build: stage the package, `Info.plist`,
/// `PkgInfo`, the icon and `Frameworks/` into the empty directory `bundle`, with
/// `artifact`'s executable as the runtime. Needs no macOS tool. Returns the bundle
/// identifier.
pub fn stage_bundle(
    project: &Project,
    artifact: &BuildArtifact,
    bundle: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
) -> Result<String> {
    stage_bundle_with_library(project, artifact, bundle, engine_root, bundle_id, None)
}

/// As [`stage_bundle`], also placing `library` in `Contents/Frameworks`.
pub fn stage_bundle_with_library(
    project: &Project,
    artifact: &BuildArtifact,
    bundle: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    library: Option<&steam::SteamLibrary>,
) -> Result<String> {
    let bundle_id = macos::bundle_identifier(&project.name, bundle_id)?;
    let package_host = app_host();
    let engine_license = engine_license(engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::App,
        steam_library: library,
    };
    stage(&inputs, bundle)?;
    macos::write_bundle_files(bundle, &project.name, &bundle_id)?;
    Ok(bundle_id)
}

/// Package a SteamPipe depot for the host platform at `destination` (a new
/// directory) from one compiled generation, for Windows and Linux. The Steam
/// backend, the manifest and the library are resolved from the process
/// ([`steam::check_depot`]); a macOS host uses [`package_steam_depot_app`].
pub fn package_steam_depot(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    depot_id: Option<u32>,
) -> Result<PathBuf> {
    let (plan, library) = steam::check_depot(project, depot_id)?;
    let host = engine_platform::PackageHost::current()
        .map_err(|error| Error::new("package.platform.unsupported", error.to_string()))?;
    package_steam_depot_with(
        project,
        artifact,
        destination,
        engine_root,
        plan,
        &host,
        &library,
    )
}

/// As [`package_steam_depot`] with the plan, host and library injected (tests run
/// every platform's layout on any OS). `host` must be Windows or Linux.
pub fn package_steam_depot_with(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    plan: steam::DepotPlan,
    host: &engine_platform::PackageHost,
    library: &steam::SteamLibrary,
) -> Result<PathBuf> {
    let platform = steam::DepotPlatform::of(host)
        .filter(|platform| *platform != steam::DepotPlatform::Macos)
        .ok_or_else(|| {
            Error::new(
                "tooling.steam.platform_unsupported",
                "A flat steam-depot is built on Windows or Linux; macOS uses the app bundle",
            )
        })?;
    let engine_license = validate_inputs(project, artifact, destination, engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: host,
        engine_license: &engine_license,
        format: PackageFormat::SteamDepot,
        steam_library: Some(library),
    };
    export_new_directory(destination, |staging| {
        let content = staging.join("content").join(platform.name());
        fs::create_dir_all(&content)?;
        stage(&inputs, &content)?;
        steam::write_depot_files(staging, plan, platform, &host.runtime_filename)
    })
    .map(|()| destination.to_path_buf())
}

/// Package a SteamPipe depot on a macOS host: the universal, signed app bundle goes
/// to `content/macos/<id>.app` (`<id>` is the sanitised project id), with Valve's
/// library in `Contents/Frameworks`. Backend, manifest and library are resolved
/// from the process.
pub fn package_steam_depot_app(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
    depot_id: Option<u32>,
) -> Result<PathBuf> {
    let (plan, library) = steam::check_depot(project, depot_id)?;
    package_steam_depot_app_with(
        project,
        builds,
        destination,
        engine_root,
        bundle_id,
        tooling,
        plan,
        &library,
    )
}

/// As [`package_steam_depot_app`] with the plan and library injected.
#[allow(clippy::too_many_arguments)]
pub fn package_steam_depot_app_with(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
    plan: steam::DepotPlan,
    library: &steam::SteamLibrary,
) -> Result<PathBuf> {
    macos::bundle_identifier(&project.name, bundle_id)?;
    check_app_builds(project, builds, destination, engine_root)?;
    let app_name = format!("{}.app", appimage::sanitize_id(&project.name));
    export_new_directory(destination, |staging| {
        let content = staging.join("content/macos");
        fs::create_dir_all(&content)?;
        let bundle = content.join(&app_name);
        fs::create_dir(&bundle)?;
        build_app(
            project,
            builds,
            &bundle,
            engine_root,
            bundle_id,
            tooling,
            Some(library),
        )?;
        steam::write_depot_files(staging, plan, steam::DepotPlatform::Macos, &app_name)
    })
    .map(|()| destination.to_path_buf())
}

/// Package as an AppImage with the host and tool already resolved (tests inject a
/// fake tool; [`package`] resolves them from the process). `destination` is the
/// `.AppImage` file to create.
pub fn package_appimage(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    tooling: &appimage::AppImageTooling,
) -> Result<PathBuf> {
    let library = steam::library_for_package(project)?;
    package_appimage_with_library(
        project,
        artifact,
        destination,
        engine_root,
        tooling,
        library.as_ref(),
    )
}

/// As [`package_appimage`] with the Steam library already resolved.
pub fn package_appimage_with_library(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    tooling: &appimage::AppImageTooling,
    library: Option<&steam::SteamLibrary>,
) -> Result<PathBuf> {
    let package_host = appimage_host();
    let engine_license = validate_inputs(project, artifact, destination, engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::AppImage,
        steam_library: library,
    };
    appimage::export_new_appimage(destination, &project.name, tooling, |appdir| {
        stage(&inputs, appdir)
    })
    .map(|()| destination.to_path_buf())
}

/// Common checks before staging; returns the canonical engine LICENSE path.
fn validate_inputs(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
) -> Result<PathBuf> {
    reject_source_destination(&project.root, destination, true)?;
    if !artifact.executable.is_file() {
        return Err(Error::new(
            "package.runtime.missing",
            "Compiled generation executable is missing",
        ));
    }
    engine_license(engine_root)
}

/// An AppImage always holds a Linux runtime, named `runtime`, whatever host
/// generated the AppDir files (the AppDir step is testable on every OS).
fn appimage_host() -> engine_platform::PackageHost {
    engine_platform::PackageHost {
        schema: 1,
        platform: engine_platform::HostPlatform::Linux,
        architecture: std::env::consts::ARCH.into(),
        runtime_filename: "runtime".into(),
        launcher_filename: "run.sh".into(),
    }
}

/// An app bundle holds one runtime for both Apple architectures, named `runtime`,
/// whatever host staged the files (the staging step is testable on every OS).
fn app_host() -> engine_platform::PackageHost {
    engine_platform::PackageHost {
        schema: 1,
        platform: engine_platform::HostPlatform::Macos,
        architecture: "universal".into(),
        runtime_filename: "runtime".into(),
        launcher_filename: "Contents/MacOS/runtime".into(),
    }
}

fn engine_license(engine_root: &Path) -> Result<PathBuf> {
    let engine_license = engine_root.canonicalize()?.join("LICENSE");
    if !engine_license.is_file() {
        return Err(Error::new(
            "package.license.missing",
            "Engine LICENSE is required",
        ));
    }
    Ok(engine_license)
}

/// The pure part of an AppImage build: stage the package and the AppImage files
/// (`AppRun`, `.desktop`, icons) into the empty directory `appdir`, as
/// `package` does before it runs `appimagetool`. Needs neither the tool nor a
/// Linux host. Returns the sanitised project id.
pub fn stage_appdir(
    project: &Project,
    artifact: &BuildArtifact,
    appdir: &Path,
    engine_root: &Path,
) -> Result<String> {
    stage_appdir_with_library(project, artifact, appdir, engine_root, None)
}

/// As [`stage_appdir`], also placing `library` beside the runtime.
pub fn stage_appdir_with_library(
    project: &Project,
    artifact: &BuildArtifact,
    appdir: &Path,
    engine_root: &Path,
    library: Option<&steam::SteamLibrary>,
) -> Result<String> {
    let package_host = appimage_host();
    let engine_license = engine_license(engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::AppImage,
        steam_library: library,
    };
    stage(&inputs, appdir)?;
    appimage::write_appdir_files(appdir, &project.name)
}

/// The top-level folder of an archive: the destination file name without the
/// format suffix (matched case-insensitively). A name left as `.` or `..`
/// (`...zip`, `..zip`) would not be a folder and is refused with
/// `tooling.destination.invalid`.
fn archive_folder(destination: &Path, format: PackageFormat) -> Result<String> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::new(
                "tooling.destination.invalid",
                "Destination requires a UTF-8 file name",
            )
        })?;
    let stem = match format.suffix() {
        Some(suffix)
            if name.len() > suffix.len()
                && name.is_char_boundary(name.len() - suffix.len())
                && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix) =>
        {
            &name[..name.len() - suffix.len()]
        }
        _ => name,
    };
    if matches!(stem, "." | "..") {
        return Err(Error::new(
            "tooling.destination.invalid",
            "Archive name must leave a folder name other than . or ..",
        ));
    }
    Ok(stem.to_owned())
}

struct Inputs<'a> {
    project: &'a Project,
    artifact: &'a BuildArtifact,
    package_host: &'a engine_platform::PackageHost,
    engine_license: &'a Path,
    format: PackageFormat,
    /// Valve's `steam_api` library, for a Steam-variant package.
    steam_library: Option<&'a steam::SteamLibrary>,
}

/// Where [`stage`] puts each part of the package, chosen by format.
struct Layout {
    runtime: PathBuf,
    /// Where the Steam library goes: where the loader finds it for this layout.
    library: PathBuf,
    game: PathBuf,
    licenses: PathBuf,
    /// `generation.json`, `package-host.json` and `PACKAGE.txt`.
    metadata: PathBuf,
    /// `run.cmd` and `run.sh` beside the runtime. A bundle starts from Finder or
    /// from its executable and has none.
    launchers: bool,
}

impl Layout {
    fn new(format: PackageFormat, root: &Path, runtime_filename: &str) -> Self {
        match format {
            PackageFormat::App => {
                let resources = root.join("Contents/Resources");
                Self {
                    runtime: root.join("Contents/MacOS").join(runtime_filename),
                    library: root.join("Contents/Frameworks"),
                    game: resources.join("game"),
                    licenses: resources.join("licenses"),
                    metadata: resources,
                    launchers: false,
                }
            }
            _ => Self {
                runtime: root.join(runtime_filename),
                library: root.to_path_buf(),
                game: root.join("game"),
                licenses: root.join("licenses"),
                metadata: root.to_path_buf(),
                launchers: true,
            },
        }
    }
}

/// Write the package into the empty directory `staging`. This one function
/// decides what is copied; [`Layout`] decides where it goes for `inputs.format`.
fn stage(inputs: &Inputs<'_>, staging: &Path) -> Result<()> {
    let Inputs {
        project,
        artifact,
        package_host,
        engine_license,
        format,
        steam_library,
    } = inputs;
    let Layout {
        runtime,
        library,
        game,
        licenses,
        metadata,
        launchers,
    } = Layout::new(*format, staging, &package_host.runtime_filename);
    for directory in [&game, &licenses, &metadata] {
        fs::create_dir_all(directory)?;
    }
    if let Some(parent) = runtime.parent() {
        fs::create_dir_all(parent)?;
    }
    copy_tree(&project.root, &game)?;
    let manifest_name = project
        .manifest
        .file_name()
        .ok_or_else(|| Error::new("package.manifest", "Manifest has no file name"))?;
    if manifest_name != "project.json" {
        fs::copy(game.join(manifest_name), game.join("project.json"))?;
    }
    Project::load(game.join("project.json"))?;
    fs::copy(&artifact.executable, &runtime)?;
    if let Some(steam_library) = steam_library {
        fs::create_dir_all(&library)?;
        steam_library.copy_into(&library)?;
    }
    fs::copy(engine_license, licenses.join("engine-LICENSE"))?;
    fs::copy(
        artifact.directory.join("Cargo.lock"),
        licenses.join("runtime-Cargo.lock"),
    )?;
    crate::artifacts::write_dependency_notices(artifact, &licenses)?;
    fs::write(
        metadata.join("generation.json"),
        serde_json::to_vec_pretty(artifact)?,
    )?;
    fs::write(
        metadata.join("package-host.json"),
        serde_json::to_vec_pretty(package_host)?,
    )?;
    let mut executables = vec![runtime];
    if launchers {
        fs::write(
            staging.join("run.cmd"),
            "@echo off\r\n\"%~dp0runtime.exe\" \"%~dp0game\\project.json\" %*\r\nexit /b %errorlevel%\r\n",
        )?;
        fs::write(
            staging.join("run.sh"),
            "#!/bin/sh\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd) || exit 3\nexec \"$HERE/runtime\" \"$HERE/game/project.json\" \"$@\"\n",
        )?;
        executables.push(staging.join("run.sh"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for executable in &executables {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::write(metadata.join("PACKAGE.txt"), package_notes(inputs))?;
    Ok(())
}

fn package_notes(inputs: &Inputs<'_>) -> String {
    let mut notes = base_notes(inputs);
    if let Some(library) = inputs.steam_library {
        notes.push_str(&format!(
            "Steam variant: {} is Valve's unmodified redistributable library, placed where the runtime loads it. It is distributed only as part of a game published through Steam under your Steamworks agreement (see docs/steam.md). No steam_appid.txt is included; Steam supplies the app id.
",
            library.file
        ));
    }
    notes
}

fn base_notes(inputs: &Inputs<'_>) -> String {
    let Inputs {
        artifact,
        package_host,
        format,
        ..
    } = inputs;
    let kind = match format {
        PackageFormat::Dir => "directory package".to_owned(),
        PackageFormat::SteamDepot => {
            "directory package staged as a SteamPipe depot; upload it with SteamCMD (see README.txt at the depot root)".to_owned()
        }
        PackageFormat::AppImage => {
            "directory package packed as an AppImage; run the .AppImage file (AppRun starts the runtime)".to_owned()
        }
        PackageFormat::App => {
            return format!(
                "Native runtime generation {} (the arm64 slice; the x86_64 slice is built from the same sources)\nFingerprint {}\nmacOS application bundle with a universal (arm64 + x86_64) runtime. Open the .app, or start Contents/MacOS/runtime; with no project argument the runtime loads Resources/game/project.json.\nThe bundle is code signed after assembly (ad-hoc unless a signing identity was configured) and is not notarised; Gatekeeper may ask the user to approve an ad-hoc bundle that was downloaded.\nPlayer saves/settings remain outside this bundle.\nDependency notices are inventoried in Resources/licenses/dependencies.json.\n",
                artifact.generation, artifact.fingerprint
            );
        }
        archive => format!(
            "directory package distributed as a {} archive; extract it before launching",
            archive.name()
        ),
    };
    format!(
        "Native runtime generation {}\nFingerprint {}\nPackaged on {} ({}). Launch {} on this host platform; the runtime also starts with no arguments and then loads game/project.json beside it.\nThis is a host-native {kind}, not a cross-platform binary, installer, signed application or store bundle.\nPlayer saves/settings remain outside this directory.\nDependency notices are inventoried in licenses/dependencies.json.\n",
        artifact.generation,
        artifact.fingerprint,
        package_host.platform.name(),
        package_host.architecture,
        package_host.launcher_filename
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_parse_and_name_round_trip() {
        for format in [
            PackageFormat::Dir,
            PackageFormat::Zip,
            PackageFormat::TarGz,
            PackageFormat::AppImage,
            PackageFormat::App,
        ] {
            assert_eq!(PackageFormat::parse(format.name()), Some(format));
        }
        assert_eq!(PackageFormat::parse("AppImage"), None);
        assert_eq!(PackageFormat::parse("App"), None);
        assert_eq!(PackageFormat::parse("tgz"), None);
        assert_eq!(PackageFormat::parse("json"), None);
    }

    #[test]
    fn archive_folder_drops_the_format_suffix_only() {
        let folder = |name: &str, format| archive_folder(Path::new(name), format).unwrap();
        assert_eq!(
            folder("out/game-1.0.tar.gz", PackageFormat::TarGz),
            "game-1.0"
        );
        assert_eq!(folder("GAME.ZIP", PackageFormat::Zip), "GAME");
        assert_eq!(folder("game.zip", PackageFormat::TarGz), "game.zip");
        assert_eq!(folder("game", PackageFormat::Zip), "game");
        assert_eq!(folder(".zip", PackageFormat::Zip), ".zip");
        for name in ["...zip", "..zip", "a/...zip"] {
            let error = archive_folder(Path::new(name), PackageFormat::Zip).unwrap_err();
            assert_eq!(error.diagnostics[0].code, "tooling.destination.invalid");
        }
        let error = archive_folder(Path::new("...tar.gz"), PackageFormat::TarGz).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "tooling.destination.invalid");
    }
}
