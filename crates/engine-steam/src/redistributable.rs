//! Where Valve's `steam_api` redistributable library lives, for a target platform.
//!
//! Pure `std` lookups shared by two consumers: this crate's `build.rs` (which
//! includes this file with `#[path]`, because a build script cannot depend on its own
//! crate) and `engine-tooling`'s package step, which stages the library next to a
//! packaged game's runtime. No file is read except to test that a candidate exists.
//!
//! Layout of both the Steamworks SDK (`<sdk>/redistributable_bin/<directory>/<file>`)
//! and the pinned `steamworks-sys` crate source
//! (`<crate>/lib/steam/redistributable_bin/<directory>/<file>`).

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

/// Name of the Steamworks SDK directory variable.
pub const SDK_LOCATION_ENV: &str = "STEAM_SDK_LOCATION";

/// The library for one platform: the `redistributable_bin` subdirectory and the file
/// name the loader asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryFile {
    pub directory: &'static str,
    pub file: &'static str,
}

/// The library for a Rust target OS (`CARGO_CFG_TARGET_OS` / `std::env::consts::OS`)
/// and architecture (`CARGO_CFG_TARGET_ARCH` / `std::env::consts::ARCH`), or `None`
/// where Valve ships none for the pairing.
pub fn library_file(os: &str, arch: &str) -> Option<LibraryFile> {
    let (directory, file) = match (os, arch) {
        ("windows", "x86_64") => ("win64", "steam_api64.dll"),
        ("linux", "x86_64") => ("linux64", "libsteam_api.so"),
        ("linux", "aarch64") => ("linuxarm64", "libsteam_api.so"),
        ("linux", "x86") => ("linux32", "libsteam_api.so"),
        // One universal (x86_64 + arm64) file serves both Apple architectures.
        ("macos", _) => ("osx", "libsteam_api.dylib"),
        _ => return None,
    };
    Some(LibraryFile { directory, file })
}

/// `<sdk>/redistributable_bin/<directory>/<file>` when it is a file.
pub fn in_sdk(sdk: &Path, library: LibraryFile) -> Option<PathBuf> {
    let path = sdk
        .join("redistributable_bin")
        .join(library.directory)
        .join(library.file);
    path.is_file().then_some(path)
}

/// The library inside the highest-versioned `steamworks-sys-<version>` source in
/// `<cargo_home>/registry/src/*/`. The version is compared numerically.
pub fn in_cargo_registry(cargo_home: &Path, library: LibraryFile) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for registry in fs::read_dir(cargo_home.join("registry").join("src"))
        .ok()?
        .flatten()
    {
        let Ok(crates) = fs::read_dir(registry.path()) else {
            continue;
        };
        for krate in crates.flatten() {
            let name = krate.file_name().to_string_lossy().into_owned();
            let Some(version) = name.strip_prefix("steamworks-sys-") else {
                continue;
            };
            let path = krate
                .path()
                .join("lib/steam/redistributable_bin")
                .join(library.directory)
                .join(library.file);
            if !path.is_file() {
                continue;
            }
            let version = parse_version(version);
            let newer = best
                .as_ref()
                .is_none_or(|(current, _)| compare(&version, current) == Ordering::Greater);
            if newer {
                best = Some((version, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Cargo's home directory: `CARGO_HOME`, else `~/.cargo` from `USERPROFILE`/`HOME`.
pub fn cargo_home(
    cargo_home: Option<&std::ffi::OsStr>,
    user_profile: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    cargo_home.map(PathBuf::from).or_else(|| {
        user_profile
            .or(home)
            .map(|h| PathBuf::from(h).join(".cargo"))
    })
}

fn parse_version(text: &str) -> Vec<u64> {
    text.split(['.', '-', '+'])
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn compare(a: &[u64], b: &[u64]) -> Ordering {
    for index in 0..a.len().max(b.len()) {
        let order = a
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(index).copied().unwrap_or(0));
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms_map_to_valves_directories() {
        let l = |os, arch| library_file(os, arch).map(|l| (l.directory, l.file));
        assert_eq!(l("windows", "x86_64"), Some(("win64", "steam_api64.dll")));
        assert_eq!(l("linux", "x86_64"), Some(("linux64", "libsteam_api.so")));
        assert_eq!(
            l("linux", "aarch64"),
            Some(("linuxarm64", "libsteam_api.so"))
        );
        assert_eq!(l("macos", "aarch64"), Some(("osx", "libsteam_api.dylib")));
        assert_eq!(l("macos", "x86_64"), Some(("osx", "libsteam_api.dylib")));
        assert_eq!(l("windows", "aarch64"), None);
        assert_eq!(l("freebsd", "x86_64"), None);
    }

    #[test]
    fn registry_lookup_prefers_the_highest_numeric_version() {
        let temp = tempfile::tempdir().unwrap();
        let library = library_file("linux", "x86_64").unwrap();
        for version in ["0.9.0", "0.13.0", "0.2.5"] {
            let dir = temp
                .path()
                .join("registry/src/index-abc")
                .join(format!("steamworks-sys-{version}"))
                .join("lib/steam/redistributable_bin/linux64");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("libsteam_api.so"), version).unwrap();
        }
        let found = in_cargo_registry(temp.path(), library).unwrap();
        assert_eq!(fs::read_to_string(found).unwrap(), "0.13.0");
        assert!(in_cargo_registry(temp.path(), library_file("macos", "x86_64").unwrap()).is_none());
        assert!(in_cargo_registry(&temp.path().join("none"), library).is_none());
    }

    #[test]
    fn sdk_lookup_needs_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let library = library_file("windows", "x86_64").unwrap();
        assert!(in_sdk(temp.path(), library).is_none());
        let dir = temp.path().join("redistributable_bin/win64");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("steam_api64.dll"), "x").unwrap();
        assert_eq!(
            in_sdk(temp.path(), library).unwrap(),
            dir.join("steam_api64.dll")
        );
    }
}
