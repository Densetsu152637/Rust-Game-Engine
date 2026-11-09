//! Lets a Steam-feature `engine` binary (and its tests) start on Linux.
//!
//! With `--features steam` the tooling links the real Steam backend, and so
//! `libsteam_api.so`, which the loader finds only through a run path. The
//! `engine-steam` build script cannot add one to its dependents, so this one points
//! the binaries at the library directory (the Steamworks SDK or the pinned
//! `steamworks-sys` source). Windows and macOS need nothing here: `engine-steam`
//! copies the library beside the binaries. Packaged games do not use this path;
//! the package step stages the library beside the runtime.

use std::env;

// The lookup is shared with `engine-steam`'s build script and the package step.
#[allow(dead_code)]
#[path = "../engine-steam/src/redistributable.rs"]
mod redistributable;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../engine-steam/src/redistributable.rs");
    println!("cargo:rerun-if-env-changed=STEAM_SDK_LOCATION");
    if env::var_os("CARGO_FEATURE_STEAM").is_none() {
        return;
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if os != "linux" {
        return;
    }
    let Some(library) = redistributable::library_file(&os, &arch) else {
        return;
    };
    let found = env::var_os(redistributable::SDK_LOCATION_ENV)
        .and_then(|sdk| redistributable::in_sdk(sdk.as_ref(), library))
        .or_else(|| {
            let home = redistributable::cargo_home(
                env::var_os("CARGO_HOME").as_deref(),
                env::var_os("USERPROFILE").as_deref(),
                env::var_os("HOME").as_deref(),
            )?;
            redistributable::in_cargo_registry(&home, library)
        });
    match found.as_deref().and_then(|path| path.parent()) {
        Some(directory) => println!("cargo:rustc-link-arg=-Wl,-rpath,{}", directory.display()),
        None => println!(
            "cargo:warning=steam_api library not found; the Steam-feature engine binary may fail to start"
        ),
    }
}
