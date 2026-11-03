//! Generated workspace contents: startup wrapper, manifest and local package roots.

use super::engine_source::GitPatch;
use super::{BuildFailure, failure, io_error};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) const STARTUP_WRAPPER: &str = r#"
mod native_entry {
    include!("entry.rs");
    pub fn invoke() { main(); }
}
fn main() {
    if let Some(ready) = std::env::var_os("ENGINE_GENERATION_READY_FILE") {
        if let Err(error) = std::fs::write(ready, b"ready") { eprintln!("generation readiness: {error}"); std::process::exit(3); }
        let mut start = String::new();
        if std::io::stdin().read_line(&mut start).is_err() || start.trim() != "start" { std::process::exit(3); }
    }
    native_entry::invoke();
}
"#;

pub(super) fn package_name(manifest: &Path) -> Result<String, BuildFailure> {
    let value: toml::Value = fs::read_to_string(manifest)
        .map_err(io_error)?
        .parse()
        .map_err(|e| failure("compiler.manifest", e))?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                "compiler.manifest",
                "External gameplay manifest requires [package].name",
            )
        })
}

pub(super) fn generated_manifest(
    package: &str,
    project: &Path,
    engine: &Path,
    features: &[String],
    binary_name: &str,
    patches: &[GitPatch],
) -> Result<String, BuildFailure> {
    let mut dependencies = toml::map::Map::new();
    let mut game = toml::map::Map::new();
    game.insert("package".into(), package.into());
    game.insert("path".into(), project.to_string_lossy().as_ref().into());
    game.insert(
        "features".into(),
        toml::Value::Array(features.iter().cloned().map(toml::Value::String).collect()),
    );
    dependencies.insert("gameplay".into(), toml::Value::Table(game));
    for name in [
        "engine-core",
        "engine-project",
        "engine-desktop",
        "engine-tooling",
    ] {
        let path = engine.join("crates").join(name);
        if path.join("Cargo.toml").is_file() {
            let mut dependency = toml::map::Map::new();
            dependency.insert("path".into(), path.to_string_lossy().as_ref().into());
            if name == "engine-tooling" && cfg!(feature = "steam") {
                // A tooling binary built with `steam` produces Steam-variant generations.
                dependency.insert(
                    "features".into(),
                    toml::Value::Array(vec![toml::Value::String("steam".into())]),
                );
            }
            dependencies.insert(name.into(), toml::Value::Table(dependency));
        }
    }
    let mut manifest = toml::map::Map::new();
    let mut package = toml::map::Map::new();
    package.insert("name".into(), "engine-runtime-generation".into());
    package.insert("version".into(), "0.0.0".into());
    package.insert("edition".into(), "2024".into());
    manifest.insert("package".into(), toml::Value::Table(package));
    let mut binary = toml::map::Map::new();
    binary.insert("name".into(), binary_name.into());
    binary.insert("path".into(), "src/main.rs".into());
    manifest.insert(
        "bin".into(),
        toml::Value::Array(vec![toml::Value::Table(binary)]),
    );
    manifest.insert("workspace".into(), toml::Value::Table(Default::default()));
    manifest.insert("dependencies".into(), toml::Value::Table(dependencies));
    if !patches.is_empty() {
        // The game's own `[patch]` is ignored under this root, so the redirect of
        // its git engine dependency to the running checkout lives here.
        let mut sources = toml::map::Map::new();
        for patch in patches {
            let mut crates = toml::map::Map::new();
            for name in &patch.crates {
                let mut entry = toml::map::Map::new();
                let path = engine.join("crates").join(name);
                entry.insert("path".into(), path.to_string_lossy().as_ref().into());
                crates.insert(name.clone(), toml::Value::Table(entry));
            }
            sources.insert(patch.url.clone(), toml::Value::Table(crates));
        }
        manifest.insert("patch".into(), toml::Value::Table(sources));
    }
    toml::to_string(&manifest).map_err(|e| failure("compiler.manifest", e))
}

pub(super) fn local_package_roots(
    metadata: &serde_json::Value,
    generation: &Path,
) -> Result<Vec<PathBuf>, BuildFailure> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| failure("compiler.metadata", "Cargo metadata has no packages"))?;
    let mut roots = Vec::new();
    for package in packages {
        if !package["source"].is_null() {
            continue;
        }
        let manifest = package["manifest_path"]
            .as_str()
            .ok_or_else(|| failure("compiler.metadata", "Package has no manifest path"))?;
        let root = Path::new(manifest)
            .parent()
            .ok_or_else(|| failure("compiler.metadata", "Package manifest has no parent"))?
            .canonicalize()
            .map_err(io_error)?;
        if root != generation {
            roots.push(root);
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}
