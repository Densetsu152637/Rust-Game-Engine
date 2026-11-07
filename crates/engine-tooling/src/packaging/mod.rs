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

pub mod archive;

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
}

impl PackageFormat {
    /// Names accepted by `--format`, for usage messages.
    pub const NAMES: &'static str = "dir|zip|tar.gz";

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "dir" => Some(Self::Dir),
            "zip" => Some(Self::Zip),
            "tar.gz" => Some(Self::TarGz),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        }
    }

    /// File name suffix of the archive, `None` for a directory.
    fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Dir => None,
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
    let package_host = engine_platform::PackageHost::current()
        .map_err(|error| Error::new("package.platform.unsupported", error.to_string()))?;
    reject_source_destination(&project.root, destination, true)?;
    if !artifact.executable.is_file() {
        return Err(Error::new(
            "package.runtime.missing",
            "Compiled generation executable is missing",
        ));
    }
    let engine_root = engine_root.canonicalize()?;
    let engine_license = engine_root.join("LICENSE");
    if !engine_license.is_file() {
        return Err(Error::new(
            "package.license.missing",
            "Engine LICENSE is required",
        ));
    }
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
        PackageFormat::Zip | PackageFormat::TarGz => {
            let folder = archive_folder(destination, format)?;
            let executables = [package_host.runtime_filename.as_str(), "run.sh"];
            archive::export_new_archive(destination, &folder, format, &executables, |staging| {
                stage(&inputs, staging)
            })
            .map(|()| destination.to_path_buf())
        }
    }
}

/// The top-level folder of an archive: the destination file name without the
/// format suffix (matched case-insensitively), or `package` when nothing is left.
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
    Ok(if stem.is_empty() {
        "package".to_owned()
    } else {
        stem.to_owned()
    })
}

struct Inputs<'a> {
    project: &'a Project,
    artifact: &'a BuildArtifact,
    package_host: &'a engine_platform::PackageHost,
    engine_license: &'a Path,
    format: PackageFormat,
}

/// Write the package layout into the empty directory `staging`.
fn stage(inputs: &Inputs<'_>, staging: &Path) -> Result<()> {
    let Inputs {
        project,
        artifact,
        package_host,
        engine_license,
        ..
    } = inputs;
    let game = staging.join("game");
    fs::create_dir(&game)?;
    copy_tree(&project.root, &game)?;
    let manifest_name = project
        .manifest
        .file_name()
        .ok_or_else(|| Error::new("package.manifest", "Manifest has no file name"))?;
    if manifest_name != "project.json" {
        fs::copy(game.join(manifest_name), game.join("project.json"))?;
    }
    Project::load(game.join("project.json"))?;
    let runtime = staging.join(&package_host.runtime_filename);
    fs::copy(&artifact.executable, &runtime)?;
    let licenses = staging.join("licenses");
    fs::create_dir(&licenses)?;
    fs::copy(engine_license, licenses.join("engine-LICENSE"))?;
    fs::copy(
        artifact.directory.join("Cargo.lock"),
        licenses.join("runtime-Cargo.lock"),
    )?;
    crate::artifacts::write_dependency_notices(artifact, &licenses)?;
    fs::write(
        staging.join("generation.json"),
        serde_json::to_vec_pretty(artifact)?,
    )?;
    fs::write(
        staging.join("package-host.json"),
        serde_json::to_vec_pretty(package_host)?,
    )?;
    fs::write(
        staging.join("run.cmd"),
        "@echo off\r\n\"%~dp0runtime.exe\" \"%~dp0game\\project.json\" %*\r\nexit /b %errorlevel%\r\n",
    )?;
    fs::write(
        staging.join("run.sh"),
        "#!/bin/sh\nHERE=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd) || exit 3\nexec \"$HERE/runtime\" \"$HERE/game/project.json\" \"$@\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for executable in [&runtime, &staging.join("run.sh")] {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::write(staging.join("PACKAGE.txt"), package_notes(inputs))?;
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
        for format in [PackageFormat::Dir, PackageFormat::Zip, PackageFormat::TarGz] {
            assert_eq!(PackageFormat::parse(format.name()), Some(format));
        }
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
    }
}
