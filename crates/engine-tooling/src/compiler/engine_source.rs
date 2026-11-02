//! Redirects a game's git dependency on the engine to the running engine checkout.
//!
//! The generated workspace links `engine-core`, `engine-project` and
//! `engine-desktop` from the engine checkout by path. A game that depends on
//! `engine` (or an `engine-*` crate) through a git source would otherwise resolve
//! a second copy of those crates, and its types would differ from the host's.
//! The generated workspace is the Cargo root, so it can carry the `[patch]` the
//! game's own manifest cannot: one entry per engine crate the game names, pointing
//! at the checkout. The game's files are never modified.

use super::{BuildFailure, failure, io_error};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One git source whose engine crates are redirected to the checkout.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct GitPatch {
    /// The URL exactly as the game spells it; Cargo canonicalises both sides.
    pub(super) url: String,
    pub(super) crates: BTreeSet<String>,
}

type Found = BTreeMap<String, (String, BTreeSet<String>)>;

/// Engine crates the game reaches through git sources, grouped by source. Local
/// path dependencies of the game are followed so a helper crate is covered too.
pub(super) fn engine_patches(
    project_manifest: &Path,
    engine_root: &Path,
) -> Result<Vec<GitPatch>, BuildFailure> {
    let mut found = Found::new();
    let mut visited = BTreeSet::new();
    scan_manifest(project_manifest, engine_root, &mut visited, &mut found)?;
    if let Some(root) = workspace_root_manifest(project_manifest) {
        scan_workspace_dependencies(&root, engine_root, &mut found)?;
    }
    let remotes = checkout_remotes(engine_root);
    let mut patches = Vec::new();
    for (normalized, (url, crates)) in found {
        if !remotes.is_empty() && !remotes.contains(&normalized) {
            return Err(failure(
                "compiler.engine_source",
                format!(
                    "The game depends on engine crates from git source `{url}`, which is not a \
                     remote of the running engine checkout ({}). Building would link two copies of \
                     the engine and split its types. Point the dependency at the running engine \
                     (a `path` dependency, or a git URL matching the checkout's remote).",
                    engine_root.display()
                ),
            ));
        }
        patches.push(GitPatch { url, crates });
    }
    Ok(patches)
}

fn scan_manifest(
    manifest: &Path,
    engine_root: &Path,
    visited: &mut BTreeSet<PathBuf>,
    found: &mut Found,
) -> Result<(), BuildFailure> {
    let manifest = manifest.canonicalize().map_err(io_error)?;
    if !visited.insert(manifest.clone()) {
        return Ok(());
    }
    let value = read_manifest(&manifest)?;
    let directory = manifest.parent().unwrap_or(Path::new("."));
    let mut tables = dependency_tables(&value);
    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            tables.extend(dependency_tables(target));
        }
    }
    let mut local = Vec::new();
    for (key, dependency) in tables.into_iter().flat_map(|table| table.iter()) {
        let Some(dependency) = dependency.as_table() else {
            continue;
        };
        if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
            let candidate = directory.join(path).join("Cargo.toml");
            if candidate.is_file() {
                local.push(candidate);
            }
        }
        record(key, dependency, engine_root, found);
    }
    for candidate in local {
        scan_manifest(&candidate, engine_root, visited, found)?;
    }
    Ok(())
}

fn scan_workspace_dependencies(
    manifest: &Path,
    engine_root: &Path,
    found: &mut Found,
) -> Result<(), BuildFailure> {
    let value = read_manifest(manifest)?;
    if let Some(table) = value
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        for (key, dependency) in table {
            if let Some(dependency) = dependency.as_table() {
                record(key, dependency, engine_root, found);
            }
        }
    }
    Ok(())
}

fn record(
    key: &str,
    dependency: &toml::map::Map<String, toml::Value>,
    engine_root: &Path,
    found: &mut Found,
) {
    let Some(url) = dependency.get("git").and_then(toml::Value::as_str) else {
        return;
    };
    let name = dependency
        .get("package")
        .and_then(toml::Value::as_str)
        .unwrap_or(key);
    let is_engine_crate = (name == "engine" || name.starts_with("engine-"))
        && engine_root
            .join("crates")
            .join(name)
            .join("Cargo.toml")
            .is_file();
    if is_engine_crate {
        found
            .entry(normalize_git_url(url))
            .or_insert_with(|| (url.to_owned(), BTreeSet::new()))
            .1
            .insert(name.to_owned());
    }
}

fn dependency_tables(value: &toml::Value) -> Vec<&toml::map::Map<String, toml::Value>> {
    ["dependencies", "dev-dependencies", "build-dependencies"]
        .iter()
        .filter_map(|name| value.get(*name).and_then(toml::Value::as_table))
        .collect()
}

fn read_manifest(path: &Path) -> Result<toml::Value, BuildFailure> {
    fs::read_to_string(path)
        .map_err(io_error)?
        .parse()
        .map_err(|e| failure("compiler.manifest", e))
}

/// The nearest `[workspace]` manifest above the game's directory.
fn workspace_root_manifest(project_manifest: &Path) -> Option<PathBuf> {
    let start = project_manifest.canonicalize().ok()?;
    start.ancestors().skip(1).find_map(|directory| {
        let candidate = directory.join("Cargo.toml");
        let text = fs::read_to_string(&candidate).ok()?;
        let value: toml::Value = text.parse().ok()?;
        value.get("workspace").map(|_| candidate)
    })
}

/// Normalised URLs of the engine checkout's git remotes. Empty when the checkout
/// is not a git repository or has none, in which case the source cannot be judged.
fn checkout_remotes(engine_root: &Path) -> BTreeSet<String> {
    if !engine_root.join(".git").exists() {
        return BTreeSet::new();
    }
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(engine_root)
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output()
    else {
        return BTreeSet::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(_, url)| normalize_git_url(url.trim()))
        .collect()
}

/// Spelling-independent form of a git URL: no scheme, user, `.git` suffix or case.
pub(super) fn normalize_git_url(url: &str) -> String {
    let mut text = url.trim().trim_start_matches("git+").to_lowercase();
    if let Some((_, rest)) = text.split_once("://") {
        text = rest.to_owned();
    }
    if let Some((_, rest)) = text.split_once('@') {
        text = rest.to_owned();
    }
    // scp-style `host:owner/repo`; `host:port` has digits after the colon.
    if let Some((host, rest)) = text.split_once(':')
        && !host.is_empty()
        && !rest.starts_with(|c: char| c.is_ascii_digit())
    {
        text = format!("{host}/{rest}");
    }
    let text = text.trim_end_matches('/');
    text.strip_suffix(".git").unwrap_or(text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::normalize_git_url;

    #[test]
    fn git_urls_compare_across_spellings() {
        let canonical = "github.com/densetsu152637/rust-game-engine";
        for spelling in [
            "https://github.com/Densetsu152637/Rust-Game-Engine.git",
            "https://github.com/Densetsu152637/Rust-Game-Engine",
            "git@github.com:Densetsu152637/Rust-Game-Engine.git",
            "ssh://git@github.com/Densetsu152637/Rust-Game-Engine.git/",
            "git+https://github.com/densetsu152637/rust-game-engine.git",
        ] {
            assert_eq!(normalize_git_url(spelling), canonical, "{spelling}");
        }
        assert_ne!(
            normalize_git_url("https://github.com/other/Rust-Game-Engine.git"),
            canonical
        );
    }
}
