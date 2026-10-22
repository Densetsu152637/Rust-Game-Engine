//! Source fingerprinting that defines generation identity.

use super::cargo::cargo_home;
use super::workspace::STARTUP_WRAPPER;
use super::{BuildFailure, CompileRequest, failure, io_error};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Environment that Cargo/rustc/build scripts treat as build configuration.
pub(super) const FINGERPRINT_VARIABLES: [&str; 11] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "CARGO_INCREMENTAL",
];
pub(super) const FINGERPRINT_PREFIXES: [&str; 5] = [
    "CARGO_BUILD_",
    "CARGO_PROFILE_",
    "CARGO_TARGET_",
    "CARGO_ENCODED_",
    "CARGO_UNSTABLE_",
];

pub(super) fn fingerprint_environment() -> BTreeMap<String, OsString> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
        .filter(|(name, _)| {
            FINGERPRINT_VARIABLES.contains(&name.as_str())
                || (FINGERPRINT_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                    // Output locations do not change compiled behavior.
                    && name != "CARGO_TARGET_DIR"
                    && name != "CARGO_TARGET_TMPDIR")
        })
        .collect()
}

pub(super) fn fingerprint(
    request: &CompileRequest,
    roots: &[PathBuf],
    generation: &Path,
    identity: &str,
    neutral: &Path,
) -> Result<String, BuildFailure> {
    let mut hash = Sha256::new();
    fn field(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    field(&mut hash, b"engine-native-generation-v2");
    field(&mut hash, identity.as_bytes());
    field(&mut hash, request.main_source.as_bytes());
    field(&mut hash, STARTUP_WRAPPER.as_bytes());
    field(
        &mut hash,
        &fs::read(generation.join("Cargo.lock")).map_err(io_error)?,
    );
    field(
        &mut hash,
        format!(
            "{:?}{:?}{}{}",
            request.features, request.target, request.release, request.offline
        )
        .as_bytes(),
    );
    for (name, value) in fingerprint_environment() {
        field(&mut hash, name.as_bytes());
        field(&mut hash, value.to_string_lossy().as_bytes());
    }
    let cache = request.cache_root.canonicalize().map_err(io_error)?;
    for root in roots {
        field(&mut hash, root.to_string_lossy().as_bytes());
        let mut files = BTreeMap::new();
        collect_sources(root, root, &cache, &mut files)?;
        for (relative, path) in files {
            field(&mut hash, relative.as_bytes());
            field(&mut hash, &fs::read(path).map_err(io_error)?);
        }
    }
    for path in configuration_inputs(neutral, roots)? {
        field(&mut hash, path.to_string_lossy().as_bytes());
        field(&mut hash, &fs::read(path).map_err(io_error)?);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Exactly the out-of-package files Cargo reads for this generation: config
/// discovered from the neutral working directory and Cargo home, plus the
/// workspace root of each local package that inherits workspace fields. Unrelated
/// ancestor workspaces/configs of the game, project or cache are not inputs.
pub(super) fn configuration_inputs(
    neutral: &Path,
    roots: &[PathBuf],
) -> Result<std::collections::BTreeSet<PathBuf>, BuildFailure> {
    let mut inputs = std::collections::BTreeSet::new();
    let mut directories = neutral
        .ancestors()
        .map(|ancestor| ancestor.join(".cargo"))
        .collect::<Vec<_>>();
    directories.extend(cargo_home());
    for directory in directories {
        for name in ["config", "config.toml"] {
            let path = directory.join(name);
            if path.is_file() {
                inputs.insert(path.canonicalize().map_err(io_error)?);
            }
        }
    }
    for root in roots {
        if let Some(workspace) = inherited_workspace(root)? {
            inputs.insert(workspace);
        }
    }
    Ok(inputs)
}

/// The workspace manifest a non-root package inherits from, mirroring Cargo's
/// lookup: an explicit `package.workspace`, otherwise the nearest ancestor
/// manifest declaring `[workspace]`. Packages that are their own workspace root
/// or inherit nothing have no external workspace input.
pub(super) fn inherited_workspace(root: &Path) -> Result<Option<PathBuf>, BuildFailure> {
    let manifest = parse_manifest(&root.join("Cargo.toml"))?;
    if manifest.get("workspace").is_some() {
        return Ok(None);
    }
    if let Some(explicit) = manifest
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str)
    {
        let path = root.join(explicit).join("Cargo.toml");
        return path.canonicalize().map(Some).map_err(|_| {
            failure(
                "compiler.workspace",
                "package.workspace manifest is missing",
            )
        });
    }
    if !inherits_workspace(&manifest) {
        return Ok(None);
    }
    for ancestor in root.ancestors().skip(1) {
        let candidate = ancestor.join("Cargo.toml");
        if candidate.is_file() && parse_manifest(&candidate)?.get("workspace").is_some() {
            return candidate.canonicalize().map(Some).map_err(io_error);
        }
    }
    Err(failure(
        "compiler.workspace",
        format!(
            "{} inherits workspace fields without a workspace root",
            root.display()
        ),
    ))
}

pub(super) fn parse_manifest(path: &Path) -> Result<toml::Value, BuildFailure> {
    fs::read_to_string(path)
        .map_err(io_error)?
        .parse()
        .map_err(|e| failure("compiler.manifest", e))
}

pub(super) fn inherits_workspace(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => table.iter().any(|(key, value)| {
            (key == "workspace" && value.as_bool() == Some(true)) || inherits_workspace(value)
        }),
        toml::Value::Array(items) => items.iter().any(inherits_workspace),
        _ => false,
    }
}

pub(super) fn collect_sources(
    root: &Path,
    directory: &Path,
    cache: &Path,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<(), BuildFailure> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        if path == cache {
            continue;
        }
        let name = entry.file_name();
        if [
            ".git", "target", "build", "dist", "release", ".derived", ".idea", ".vs",
        ]
        .iter()
        .any(|skip| name == *skip)
            || name.to_string_lossy().starts_with("cmake-build-")
        {
            continue;
        }
        let canonical = path.canonicalize().map_err(io_error)?;
        if !canonical.starts_with(root) {
            return Err(failure(
                "compiler.path",
                format!("Source link escapes root: {}", path.display()),
            ));
        }
        let metadata = entry.path().symlink_metadata().map_err(io_error)?;
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let linked = metadata.file_type().is_symlink();
        if linked {
            // Reject directory aliases rather than recurse through cycles or
            // miss changes through an external link.
            return Err(failure(
                "compiler.path",
                format!("Source symlinks are unsupported: {}", path.display()),
            ));
        }
        if canonical.is_dir() {
            collect_sources(root, &canonical, cache, files)?;
        } else if canonical.is_file() {
            files.insert(
                path.strip_prefix(root)
                    .map_err(|e| failure("compiler.path", e))?
                    .to_string_lossy()
                    .replace('\\', "/"),
                canonical,
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod configuration_inputs_tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn package_without_inheritance_ignores_ancestor_workspace() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
        let game = temp.path().join("game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion='0.1.0'\n",
        );
        assert_eq!(inherited_workspace(&game).unwrap(), None);
    }

    #[test]
    fn inheriting_package_uses_nearest_ancestor_workspace() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
        write(
            &temp.path().join("outer/Cargo.toml"),
            "[workspace]\nmembers=['game']\n[workspace.package]\nversion='1.0.0'\n",
        );
        let game = temp.path().join("outer/game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion.workspace=true\n[dependencies]\nserde={workspace=true}\n",
        );
        assert_eq!(
            inherited_workspace(&game).unwrap(),
            Some(temp.path().join("outer/Cargo.toml").canonicalize().unwrap())
        );
    }

    #[test]
    fn explicit_workspace_key_and_own_root_are_respected() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("elsewhere/Cargo.toml"), "[workspace]\n");
        let explicit = temp.path().join("explicit");
        write(
            &explicit.join("Cargo.toml"),
            "[package]\nname='a'\nversion='0.1.0'\nworkspace='../elsewhere'\n",
        );
        assert_eq!(
            inherited_workspace(&explicit).unwrap(),
            Some(
                temp.path()
                    .join("elsewhere/Cargo.toml")
                    .canonicalize()
                    .unwrap()
            )
        );
        let own = temp.path().join("elsewhere/own");
        write(
            &own.join("Cargo.toml"),
            "[package]\nname='b'\nversion.workspace=true\n[workspace]\n[workspace.package]\nversion='1.0.0'\n",
        );
        assert_eq!(inherited_workspace(&own).unwrap(), None);
    }

    #[test]
    fn inheritance_without_any_workspace_is_a_diagnosed_failure() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion='0.1.0'\n[lints]\nworkspace=true\n",
        );
        // The temp directory's ancestors must not declare a workspace for this
        // negative case; skip rather than misreport on unusual hosts.
        if temp
            .path()
            .ancestors()
            .skip(1)
            .any(|a| a.join("Cargo.toml").is_file())
        {
            return;
        }
        assert_eq!(
            inherited_workspace(&game).unwrap_err().code,
            "compiler.workspace"
        );
    }

    #[test]
    fn output_location_variables_do_not_affect_identity() {
        let environment = fingerprint_environment();
        assert!(!environment.contains_key("CARGO_TARGET_DIR"));
        assert!(!environment.contains_key("CARGO_TARGET_TMPDIR"));
        assert!(!environment.contains_key("CARGO_MANIFEST_DIR"));
    }
}
