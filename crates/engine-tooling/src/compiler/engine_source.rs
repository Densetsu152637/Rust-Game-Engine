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
    /// The URL as the game spells it (the first spelling Cargo treats as this source).
    pub(super) url: String,
    pub(super) crates: BTreeSet<String>,
}

/// Git sources found, by normalised URL, then by Cargo's own notion of the source
/// (see [`cargo_source_key`]): the first spelling used and the crates taken from it.
type Found = BTreeMap<String, BTreeMap<String, (String, BTreeSet<String>)>>;
type Table = toml::map::Map<String, toml::Value>;

/// The redirects to emit and any non-fatal findings to report with the build.
#[derive(Debug, Default)]
pub(super) struct EnginePatches {
    pub(super) patches: Vec<GitPatch>,
    pub(super) warnings: Vec<String>,
}

/// Engine crates the game reaches through git sources, grouped by source. Local
/// path dependencies of the game are followed so a helper crate is covered too,
/// and `workspace = true` entries are resolved in the workspace of the crate that
/// inherits them. Each source Cargo distinguishes (`https` versus `ssh`) gets its own patch.
pub(super) fn engine_patches(
    project_manifest: &Path,
    engine_root: &Path,
) -> Result<EnginePatches, BuildFailure> {
    let mut found = Found::new();
    let mut visited = BTreeSet::new();
    scan_manifest(project_manifest, engine_root, &mut visited, &mut found)?;
    let remotes = checkout_remotes(engine_root);
    let mut result = EnginePatches::default();
    for (normalized, spellings) in found {
        for (url, crates) in spellings.into_values() {
            if remotes.is_empty() {
                result.warnings.push(format!(
                    "The engine checkout {} has no git remote to compare, so git source `{url}` \
                     is assumed to be this engine and its crates are redirected to the checkout.",
                    engine_root.display()
                ));
            } else if !remotes.contains(&normalized) {
                return Err(failure(
                    "compiler.engine_source",
                    format!(
                        "The game depends on engine crates from git source `{url}`, which is not \
                         a remote of the running engine checkout ({}). Building would link two \
                         copies of the engine and split its types. Point the dependency at the \
                         running engine (a `path` dependency, or a git URL matching the \
                         checkout's remote).",
                        engine_root.display()
                    ),
                ));
            }
            result.patches.push(GitPatch { url, crates });
        }
    }
    Ok(result)
}

/// A compiler-style warning diagnostic for a finding from [`engine_patches`].
pub(super) fn engine_source_warning(message: &str) -> serde_json::Value {
    serde_json::json!({
        "level": "warning",
        "message": message,
        "code": { "code": "compiler.engine_source_unverified" },
        "rendered": format!("warning: {message}
    "),
    })
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
    let inherited = workspace_dependencies(&manifest, &value);
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
        let inherits = dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true);
        let (dependency, base) = if inherits {
            // Only entries a scanned crate actually inherits count.
            let entry = inherited.as_ref().and_then(|(table, base)| {
                let entry = table.get(key)?.as_table()?;
                Some((entry, base.as_path()))
            });
            match entry {
                Some(entry) => entry,
                None => continue,
            }
        } else {
            (dependency, directory)
        };
        if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
            let candidate = base.join(path).join("Cargo.toml");
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

/// `[workspace.dependencies]` of the crate's own workspace root (the crate itself
/// or the nearest ancestor with `[workspace]`) and that root's directory. A
/// malformed ancestor manifest is ignored: it matters only if inherited from.
fn workspace_dependencies(manifest: &Path, own: &toml::Value) -> Option<(Table, PathBuf)> {
    let from = |value: &toml::Value, directory: &Path| {
        value.get("workspace").map(|workspace| {
            let table = workspace
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            (table, directory.to_path_buf())
        })
    };
    if let Some(found) = from(own, manifest.parent()?) {
        return Some(found);
    }
    manifest.ancestors().skip(2).find_map(|directory| {
        let text = fs::read_to_string(directory.join("Cargo.toml")).ok()?;
        let value: toml::Value = text.parse().ok()?;
        from(&value, directory)
    })
}

fn record(key: &str, dependency: &Table, engine_root: &Path, found: &mut Found) {
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
            .or_default()
            .entry(cargo_source_key(url))
            .or_insert_with(|| (url.to_owned(), BTreeSet::new()))
            .1
            .insert(name.to_owned());
    }
}

/// Spellings Cargo treats as one source (trailing `/`, `.git`) must share one
/// `[patch]` table, because Cargo keys patches by its canonical URL; spellings it
/// keeps distinct (`https` versus `ssh`) each need their own.
fn cargo_source_key(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url).to_owned()
}

fn dependency_tables(value: &toml::Value) -> Vec<&Table> {
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
/// `file://` URLs and local paths compare as the same lower-cased path.
pub(super) fn normalize_git_url(url: &str) -> String {
    let mut text = url.trim().trim_start_matches("git+").to_lowercase();
    if let Some(rest) = text.strip_prefix("file://") {
        text = rest.to_owned();
    }
    text = text.replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_owned();
    }
    let drive = |t: &str| {
        let b = t.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
    };
    // `file:///d:/x` leaves `/d:/x`.
    if text.starts_with('/') && drive(&text[1..]) {
        text.remove(0);
    }
    let local = text.starts_with('/') || text.starts_with("./") || drive(&text);
    if !local {
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
    }
    let text = text.trim_end_matches('/');
    text.strip_suffix(".git").unwrap_or(text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/o/engine.git";

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A checkout with `engine`, `engine-core` and `engine-project` crates.
    fn checkout(root: &Path) -> PathBuf {
        let engine = root.join("engine");
        for name in ["engine", "engine-core", "engine-project"] {
            write(
                &engine.join("crates").join(name).join("Cargo.toml"),
                &format!("[package]\nname='{name}'\n"),
            );
        }
        engine
    }

    fn game(root: &Path, relative: &str, manifest: &str) -> PathBuf {
        let path = root.join(relative).join("Cargo.toml");
        write(&path, manifest);
        path
    }

    fn crates(patches: &EnginePatches) -> Vec<(String, Vec<String>)> {
        patches
            .patches
            .iter()
            .map(|p| (p.url.clone(), p.crates.iter().cloned().collect()))
            .collect()
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }

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

    #[test]
    fn file_urls_and_local_paths_compare_equal() {
        assert_eq!(
            normalize_git_url("file:///D:/x/Engine"),
            normalize_git_url("D:/x/engine")
        );
        assert_eq!(
            normalize_git_url("file:///D:/x/engine"),
            normalize_git_url(r"D:\x\engine")
        );
        assert_eq!(
            normalize_git_url(r"\\?\D:\x\engine"),
            normalize_git_url("file:///D:/x/engine/")
        );
        assert_eq!(
            normalize_git_url("file:///tmp/x"),
            normalize_git_url("/tmp/x")
        );
        assert_eq!(normalize_git_url("file:///tmp/x.git"), "/tmp/x");
        assert_ne!(
            normalize_git_url("file:///tmp/x"),
            normalize_git_url("/tmp/y")
        );
        // A drive path is not mistaken for an scp-style remote.
        assert_ne!(normalize_git_url("D:/x"), normalize_git_url("d/x"));
    }

    #[test]
    fn workspace_inheritance_renames_and_every_dependency_table_are_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(
            dir.path(),
            "ws/game",
            &format!(
                "[package]\nname='g'\n\
                 [dependencies]\nengine={{workspace=true}}\n\
                 renamed={{package='engine-core',git='{URL}',tag='t'}}\n\
                 [dev-dependencies]\nengine-project={{git='{URL}',tag='t'}}\n\
                 [target.'cfg(windows)'.build-dependencies]\nengine-core={{git='{URL}',tag='t'}}\n"
            ),
        );
        write(
            &dir.path().join("ws/Cargo.toml"),
            &format!(
                "[workspace]\nmembers=['game']\n[workspace.dependencies]\nengine={{git='{URL}',tag='t'}}\n"
            ),
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        assert_eq!(
            crates(&patches),
            [(
                URL.to_owned(),
                vec![
                    "engine".into(),
                    "engine-core".into(),
                    "engine-project".into()
                ]
            )]
        );
    }

    #[test]
    fn path_dependencies_are_followed_including_cycles_and_their_own_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(
            dir.path(),
            "a",
            "[package]\nname='a'\n[dependencies]\nb={path='../b'}\n",
        );
        // b depends back on a (a cycle) and inherits from its own, separate workspace.
        write(
            &dir.path().join("b/Cargo.toml"),
            &format!(
                "[package]\nname='b'\n[workspace]\n[dependencies]\na={{path='../a'}}\nengine-core={{workspace=true}}\n[workspace.dependencies]\nengine-core={{git='{URL}'}}\n"
            ),
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        assert_eq!(
            crates(&patches),
            [(URL.to_owned(), vec!["engine-core".to_owned()])]
        );
    }

    #[test]
    fn malformed_manifest_is_a_diagnostic_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(dir.path(), "g", "[package\nname=");
        let error = engine_patches(&manifest, &engine).unwrap_err();
        assert_eq!(error.code, "compiler.manifest");
        // A malformed path dependency is reported too.
        let manifest = game(
            dir.path(),
            "h",
            "[package]\nname='h'\n[dependencies]\nbad={path='../bad'}\n",
        );
        write(&dir.path().join("bad/Cargo.toml"), "[[[");
        assert_eq!(
            engine_patches(&manifest, &engine).unwrap_err().code,
            "compiler.manifest"
        );
    }

    #[test]
    fn unrelated_git_crates_get_no_patch_and_no_error() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(
            dir.path(),
            "g",
            "[package]\nname='g'\n[dependencies]\n\
             serde={git='https://example.com/serde.git'}\n\
             engine-other={git='https://example.com/other.git'}\n\
             engine-core={path='../engine/crates/engine-core'}\n",
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        assert!(patches.patches.is_empty());
        assert!(patches.warnings.is_empty());
    }

    #[test]
    fn unused_workspace_entries_never_fail_but_inherited_foreign_ones_do() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        git(&engine, &["init", "-q"]);
        git(&engine, &["remote", "add", "origin", URL]);
        let workspace = "[workspace]\nmembers=['g']\n[workspace.dependencies]\n\
                         engine-core={git='https://elsewhere.example/o/engine.git'}\n";
        write(&dir.path().join("ws/Cargo.toml"), workspace);
        let unused = game(dir.path(), "ws/g", "[package]\nname='g'\n");
        let patches = engine_patches(&unused, &engine).unwrap();
        assert!(patches.patches.is_empty());
        let inherited = game(
            dir.path(),
            "ws/g",
            "[package]\nname='g'\n[dependencies]\nengine-core={workspace=true}\n",
        );
        assert_eq!(
            engine_patches(&inherited, &engine).unwrap_err().code,
            "compiler.engine_source"
        );
    }

    #[test]
    fn matching_remote_in_another_spelling_is_accepted_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        git(&engine, &["init", "-q"]);
        git(
            &engine,
            &["remote", "add", "origin", "git@example.com:o/engine.git"],
        );
        let manifest = game(
            dir.path(),
            "g",
            &format!("[package]\nname='g'\n[dependencies]\nengine={{git='{URL}'}}\n"),
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        assert_eq!(patches.patches.len(), 1);
        assert!(patches.warnings.is_empty());
    }

    #[test]
    fn checkout_without_remotes_redirects_with_a_warning_naming_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(
            dir.path(),
            "g",
            &format!("[package]\nname='g'\n[dependencies]\nengine={{git='{URL}'}}\n"),
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        assert_eq!(patches.patches.len(), 1);
        assert_eq!(patches.warnings.len(), 1);
        assert!(patches.warnings[0].contains(URL), "{:?}", patches.warnings);
        // A git repository with no remote is judged the same way.
        git(&engine, &["init", "-q"]);
        assert_eq!(
            engine_patches(&manifest, &engine).unwrap().warnings.len(),
            1
        );
    }

    #[test]
    fn each_spelling_of_one_repository_gets_its_own_patch() {
        let dir = tempfile::tempdir().unwrap();
        let engine = checkout(dir.path());
        let manifest = game(
            dir.path(),
            "g",
            "[package]\nname='g'\n[dependencies]\n\
             engine={git='https://example.com/o/engine.git'}\n\
             engine-core={git='git@example.com:o/engine.git'}\n",
        );
        let patches = engine_patches(&manifest, &engine).unwrap();
        let urls: Vec<_> = patches.patches.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "git@example.com:o/engine.git",
                "https://example.com/o/engine.git"
            ]
        );
        assert_eq!(patches.warnings.len(), 2);
    }
}
