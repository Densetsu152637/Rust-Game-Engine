//! Stages Valve's `steam_api` library where test and example executables can load it.
//!
//! `steamworks-sys` links `steam_api64.dll` / `libsteam_api.so` / `libsteam_api.dylib`
//! dynamically, so any executable using the `steam` feature needs it at start-up.
//! Cargo only places it in a build-script output directory, which is not on the
//! loader path. With the `steam` feature this script copies the library from the
//! pinned `steamworks-sys` source (Windows) or adds an rpath to it (Linux, macOS)
//! for test and example binaries. Released games must ship the library themselves.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=STEAM_SDK_LOCATION");
    if env::var_os("CARGO_FEATURE_STEAM").is_none() {
        return;
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let (subdir, file) = match os.as_str() {
        "windows" => ("win64", "steam_api64.dll"),
        "linux" => ("linux64", "libsteam_api.so"),
        "macos" => ("osx", "libsteam_api.dylib"),
        _ => return,
    };
    let Some(lib_dir) = find_library_dir(subdir, file) else {
        println!(
            "cargo:warning=steam_api library not found; Steam test executables may fail to start"
        );
        return;
    };
    if os == "windows" {
        // OUT_DIR is <target>/<profile>/build/<pkg>/out.
        let out = PathBuf::from(env::var("OUT_DIR").unwrap_or_default());
        let Some(profile_dir) = out.ancestors().nth(3) else {
            return;
        };
        for dest in ["", "deps", "examples"] {
            let dir = profile_dir.join(dest);
            if dir.is_dir() {
                let _ = fs::copy(lib_dir.join(file), dir.join(file));
            }
        }
    } else {
        let rpath = format!("-Wl,-rpath,{}", lib_dir.display());
        println!("cargo:rustc-link-arg-tests={rpath}");
        println!("cargo:rustc-link-arg-examples={rpath}");
    }
}

/// Directory holding the redistributable library for this platform.
fn find_library_dir(subdir: &str, file: &str) -> Option<PathBuf> {
    if let Some(sdk) = env::var_os("STEAM_SDK_LOCATION") {
        let dir = Path::new(&sdk).join("redistributable_bin").join(subdir);
        if dir.join(file).is_file() {
            return Some(dir);
        }
    }
    let cargo_home = env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE")
            .or_else(|| env::var_os("HOME"))
            .map(|home| PathBuf::from(home).join(".cargo"))
    })?;
    let mut best: Option<PathBuf> = None;
    for registry in fs::read_dir(cargo_home.join("registry").join("src"))
        .ok()?
        .flatten()
    {
        for krate in fs::read_dir(registry.path()).ok()?.flatten() {
            let name = krate.file_name().to_string_lossy().into_owned();
            if !name.starts_with("steamworks-sys-") {
                continue;
            }
            let dir = krate
                .path()
                .join("lib/steam/redistributable_bin")
                .join(subdir);
            if dir.join(file).is_file() && best.as_ref().is_none_or(|b| dir > *b) {
                best = Some(dir);
            }
        }
    }
    best
}
