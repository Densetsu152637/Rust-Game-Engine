//! Stages Valve's `steam_api` library where test and example executables can load it.
//!
//! `steamworks-sys` links `steam_api64.dll` / `libsteam_api.so` / `libsteam_api.dylib`
//! dynamically, so any executable using the `steam` feature needs it at start-up.
//! Cargo only places it in a build-script output directory, which is not on the
//! loader path. With the `steam` feature this script copies the library from the
//! pinned `steamworks-sys` source (Windows, macOS) or adds an rpath to it (Linux)
//! for test and example binaries. Released games get the library from the package
//! step (`engine-tooling`'s `packaging::steam`).

use std::env;
use std::fs;
use std::path::PathBuf;

// The lookup is shared with `engine-tooling`'s package step (a build script cannot
// depend on its own crate, so the file is included).
#[allow(dead_code)]
#[path = "src/redistributable.rs"]
mod redistributable;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/redistributable.rs");
    println!("cargo:rerun-if-env-changed=STEAM_SDK_LOCATION");
    if env::var_os("CARGO_FEATURE_STEAM").is_none() {
        return;
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let Some(library) = redistributable::library_file(&os, &arch) else {
        return;
    };
    let Some(path) = find_library(library) else {
        println!(
            "cargo:warning=steam_api library not found; Steam test executables may fail to start"
        );
        return;
    };
    let lib_dir = path.parent().map(PathBuf::from).unwrap_or_default();
    let file = library.file;
    if os == "windows" || os == "macos" {
        // Windows loads a DLL from the executable's directory. A macOS client records
        // the dylib's own install name, `@loader_path/libsteam_api.dylib`, so the
        // loader looks beside the executable as well and an rpath would not help.
        // OUT_DIR is <target>/<profile>/build/<pkg>/out.
        let out = PathBuf::from(env::var("OUT_DIR").unwrap_or_default());
        let Some(profile_dir) = out.ancestors().nth(3) else {
            return;
        };
        for dest in ["", "deps", "examples"] {
            let dir = profile_dir.join(dest);
            if dir.is_dir() {
                let _ = fs::copy(lib_dir.join(file), dir.join(file));
                // A restored build cache keeps this script's result but not the copy;
                // a missing copy makes Cargo run the script again.
                println!("cargo:rerun-if-changed={}", dir.join(file).display());
            }
        }
    } else {
        let rpath = format!("-Wl,-rpath,{}", lib_dir.display());
        // A plain link argument, not `-tests`/`-examples`: Cargo rejects a per-kind
        // instruction for a target kind the package does not have (this crate has no
        // examples), and the plain one covers the lib's unit tests and tests/ as well.
        println!("cargo:rustc-link-arg={rpath}");
    }
}

/// The redistributable library for this platform: the SDK directory, else the
/// pinned `steamworks-sys` source.
fn find_library(library: redistributable::LibraryFile) -> Option<PathBuf> {
    if let Some(sdk) = env::var_os(redistributable::SDK_LOCATION_ENV)
        && let Some(path) = redistributable::in_sdk(sdk.as_ref(), library)
    {
        return Some(path);
    }
    let cargo_home = redistributable::cargo_home(
        env::var_os("CARGO_HOME").as_deref(),
        env::var_os("USERPROFILE").as_deref(),
        env::var_os("HOME").as_deref(),
    )?;
    redistributable::in_cargo_registry(&cargo_home, library)
}
