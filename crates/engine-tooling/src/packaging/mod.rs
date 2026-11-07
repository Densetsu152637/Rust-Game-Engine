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

pub mod appimage;
pub mod archive;
pub mod macos;

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
}

impl PackageFormat {
    /// Names accepted by `--format`, for usage messages.
    pub const NAMES: &'static str = "dir|zip|tar.gz|appimage|app";

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dir" => Some(Self::Dir),
            "zip" => Some(Self::Zip),
            "tar.gz" => Some(Self::TarGz),
            "appimage" => Some(Self::AppImage),
            "app" => Some(Self::App),
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
        }
    }

    /// File name suffix of the archive, `None` for a directory.
    fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Dir | Self::AppImage | Self::App => None,
            Self::Zip => Some(".zip"),
            Self::TarGz => Some(".tar.gz"),
        }
    }
}

/// Package the already compiled native game in `format` at `destination`.
/// Launchers require neither Cargo nor source checkout paths. Gameplay sources
/// remain because authored script assets are stable content references, while
/// native execution uses the copied binary.
pub fn package(
    project: &Project,
    artifact: &BuildArtifact,
    destination: &Path,
    engine_root: &Path,
    format: PackageFormat,
) -> Result<PathBuf> {
    if format == PackageFormat::App {
        return Err(Error::new(
            "package.format.app",
            "--format app needs one build per architecture; use package_app",
        ));
    }
    if format == PackageFormat::AppImage {
        // The host and tool are checked before anything is staged.
        let tooling = appimage::AppImageTooling::from_process()?;
        return package_appimage(project, artifact, destination, engine_root, &tooling);
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
    };
    match format {
        PackageFormat::Dir => export_new_directory(destination, |staging| stage(&inputs, staging))
            .map(|()| destination.to_path_buf()),
        PackageFormat::AppImage | PackageFormat::App => unreachable!("returned above"),
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
        PackageFormat::Dir | PackageFormat::Zip | PackageFormat::TarGz => {}
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
/// two runtimes with `lipo`, sign it and publish it with a no-clobber rename.
pub fn package_app(
    project: &Project,
    builds: &AppBuilds<'_>,
    destination: &Path,
    engine_root: &Path,
    bundle_id: Option<&str>,
    tooling: &macos::MacosTooling,
) -> Result<PathBuf> {
    macos::bundle_file_stem(destination)?;
    macos::bundle_identifier(&project.name, bundle_id)?;
    if !builds.x86_64.executable.is_file() {
        return Err(Error::new(
            "package.runtime.missing",
            "Compiled x86_64 generation executable is missing",
        ));
    }
    validate_inputs(project, builds.arm64, destination, engine_root)?;
    macos::export_new_app(destination, |bundle| {
        stage_bundle(project, builds.arm64, bundle, engine_root, bundle_id)?;
        macos::make_universal(
            tooling,
            &builds.arm64.executable,
            &builds.x86_64.executable,
            bundle,
        )?;
        macos::strip_extended_attributes(tooling, bundle)?;
        macos::sign_bundle(tooling, bundle)
    })
    .map(|()| destination.to_path_buf())
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
    let bundle_id = macos::bundle_identifier(&project.name, bundle_id)?;
    let package_host = app_host();
    let engine_license = engine_license(engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::App,
    };
    stage(&inputs, bundle)?;
    macos::write_bundle_files(bundle, &project.name, &bundle_id)?;
    Ok(bundle_id)
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
    let package_host = appimage_host();
    let engine_license = validate_inputs(project, artifact, destination, engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::AppImage,
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
    let package_host = appimage_host();
    let engine_license = engine_license(engine_root)?;
    let inputs = Inputs {
        project,
        artifact,
        package_host: &package_host,
        engine_license: &engine_license,
        format: PackageFormat::AppImage,
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
}

/// Where [`stage`] puts each part of the package, chosen by format.
struct Layout {
    runtime: PathBuf,
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
                    game: resources.join("game"),
                    licenses: resources.join("licenses"),
                    metadata: resources,
                    launchers: false,
                }
            }
            _ => Self {
                runtime: root.join(runtime_filename),
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
    } = inputs;
    let Layout {
        runtime,
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
    let Inputs {
        artifact,
        package_host,
        format,
        ..
    } = inputs;
    let kind = match format {
        PackageFormat::Dir => "directory package".to_owned(),
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
