//! A game that depends on the `engine` facade by path or by git links exactly one
//! copy of the engine crates in the generated workspace.

use engine_tooling::compiler::{CompileRequest, NativeCompiler};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn write(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(arguments)
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?}");
}

/// A miniature engine workspace (`engine-core`, `engine-project`, the `engine`
/// facade) that is also a git repository tagged `v0.1.0`, and a game using it.
struct Fixture {
    root: PathBuf,
    engine: PathBuf,
    game: PathBuf,
}

impl Fixture {
    fn new(dependency: impl Fn(&Path) -> String) -> Self {
        // Kept short for the same MAX_PATH reasons as the native compiler tests.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tooling-fixtures")
            .join(format!(
                "git-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let engine = root.join("engine");
        write(
            &engine.join("Cargo.toml"),
            "[workspace]\nmembers=['crates/*']\nresolver='3'\n",
        );
        write(
            &engine.join("crates/engine-core/Cargo.toml"),
            "[package]\nname='engine-core'\nversion='0.1.0'\nedition='2024'\n",
        );
        write(
            &engine.join("crates/engine-core/src/lib.rs"),
            "#[derive(Default)] pub struct EngineApp { pub value: i32 }\n",
        );
        write(
            &engine.join("crates/engine-project/Cargo.toml"),
            "[package]\nname='engine-project'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nengine-core={path='../engine-core'}\n",
        );
        write(
            &engine.join("crates/engine-project/src/lib.rs"),
            "pub use engine_core::EngineApp;\n",
        );
        write(
            &engine.join("crates/engine/Cargo.toml"),
            "[package]\nname='engine'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nengine-core={path='../engine-core'}\nengine-project={path='../engine-project'}\n",
        );
        write(
            &engine.join("crates/engine/src/lib.rs"),
            "pub use engine_core as core;\npub use engine_project as project;\n",
        );
        write(&engine.join(".gitignore"), "target\n");
        git(&engine, &["init", "-q"]);
        git(&engine, &["add", "."]);
        git(&engine, &["commit", "-q", "-m", "engine"]);
        git(&engine, &["tag", "v0.1.0"]);
        let game = root.join("game");
        write(
            &game.join("Cargo.toml"),
            &format!(
                "[package]\nname='external-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\n{}\n",
                dependency(&engine)
            ),
        );
        write(
            &game.join("src/lib.rs"),
            "pub fn configure(app: &mut engine::core::EngineApp) -> Result<(),String> { app.value=7; Ok(()) }\n",
        );
        Self { root, engine, game }
    }

    fn request(&self) -> CompileRequest {
        CompileRequest {
            engine_root: self.engine.clone(),
            project_manifest: self.game.join("Cargo.toml"),
            cache_root: self.root.join("cache"),
            // The host links the engine crates directly; a second copy of them
            // behind the game's `engine` would make this a type error.
            main_source: "fn main() { let mut app=engine_project::EngineApp::default(); gameplay::configure(&mut app).unwrap(); assert_eq!(app.value,7); }".into(),
            features: vec![],
            target: None,
            release: false,
            offline: false,
            target_dir: Some(self.root.join("target")),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn compiler() -> NativeCompiler {
    let cargo = PathBuf::from(env!("CARGO"));
    let rustc = cargo
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    NativeCompiler::new(cargo, rustc)
}

const FACADE: &[&str] = &["engine", "engine-project", "engine-core"];

/// `file:///D:/x` on Windows and `file:///tmp/x` elsewhere.
fn file_url(path: &Path) -> String {
    let path = slashes(path);
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn git_dependency(engine: &Path) -> String {
    format!("engine={{git=\"{}\",tag=\"v0.1.0\"}}", file_url(engine))
}

fn path_dependency(engine: &Path) -> String {
    format!(
        "engine={{path={:?}}}",
        slashes(&engine.join("crates/engine"))
    )
}

/// Package sources recorded for `name` in the generated workspace's metadata.
fn packages(directory: &Path, name: &str) -> Vec<serde_json::Value> {
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("cargo-metadata.json")).unwrap()).unwrap();
    metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| package["name"] == name)
        .cloned()
        .collect()
}

fn assert_single_engine_copy(fixture: &Fixture, directory: &Path, names: &[&str]) {
    for name in names {
        let found = packages(directory, name);
        assert_eq!(found.len(), 1, "{name}: {found:?}");
        assert!(
            found[0]["source"].is_null(),
            "{name} must be the local checkout"
        );
        let manifest = slashes(Path::new(found[0]["manifest_path"].as_str().unwrap()));
        assert!(
            manifest.starts_with(&slashes(&fixture.engine)),
            "{name} resolved to {manifest}"
        );
    }
    // The same property through Cargo's own graph view of the generated workspace.
    let tree = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "--color",
            "never",
            "--prefix",
            "none",
            "--manifest-path",
        ])
        .arg(directory.join("Cargo.toml"))
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    let tree = String::from_utf8_lossy(&tree.stdout);
    let count = tree
        .lines()
        .filter(|line| line.starts_with("engine-project "))
        .map(|line| line.trim_end_matches(" (*)"))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(count, 1, "cargo tree:\n{tree}");
}

#[test]
fn git_dependency_on_the_facade_links_one_engine_copy() {
    let fixture = Fixture::new(git_dependency);
    let artifact = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap();
    assert_single_engine_copy(&fixture, &artifact.directory, FACADE);
    assert!(
        Command::new(&artifact.executable)
            .status()
            .unwrap()
            .success()
    );
    let manifest = fs::read_to_string(artifact.directory.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("[patch."), "{manifest}");
    // The game's own manifest is untouched.
    assert!(
        fs::read_to_string(fixture.game.join("Cargo.toml"))
            .unwrap()
            .contains("tag=\"v0.1.0\"")
    );
}

#[test]
fn git_dependencies_on_individual_engine_crates_are_redirected() {
    let fixture = Fixture::new(|engine| {
        let url = file_url(engine);
        format!(
            "engine-core={{git=\"{url}\",tag=\"v0.1.0\"}}\nengine-project={{git=\"{url}\",tag=\"v0.1.0\"}}"
        )
    });
    fs::write(
        fixture.game.join("src/lib.rs"),
        "pub fn configure(app: &mut engine_core::EngineApp) -> Result<(),String> { app.value=7; Ok(()) }\n",
    )
    .unwrap();
    let artifact = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap();
    assert_single_engine_copy(
        &fixture,
        &artifact.directory,
        &["engine-project", "engine-core"],
    );
    assert!(
        Command::new(&artifact.executable)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn path_dependency_on_the_facade_links_one_engine_copy() {
    let fixture = Fixture::new(path_dependency);
    let artifact = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap();
    assert_single_engine_copy(&fixture, &artifact.directory, FACADE);
    assert!(
        Command::new(&artifact.executable)
            .status()
            .unwrap()
            .success()
    );
    let manifest = fs::read_to_string(artifact.directory.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("[patch"), "{manifest}");
}

#[test]
fn git_source_that_is_not_the_running_engine_is_reported() {
    let fixture = Fixture::new(git_dependency);
    // The running checkout now claims a different origin than the game's source,
    // so linking the checkout would split the game's engine types.
    git(
        &fixture.engine,
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/real/engine.git",
        ],
    );
    let error = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(error.code, "compiler.engine_source", "{error}");
    assert!(error.message.contains("path"), "{}", error.message);
}

fn unverified_warnings(artifact: &engine_tooling::compiler::BuildArtifact) -> usize {
    artifact
        .diagnostics
        .iter()
        .filter(|d| d["code"]["code"] == "compiler.engine_source_unverified")
        .count()
}

#[test]
fn checkout_without_remotes_warns_naming_the_git_source() {
    let fixture = Fixture::new(git_dependency);
    let artifact = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(unverified_warnings(&artifact), 1);
    let message = artifact.diagnostics[0]["message"].as_str().unwrap();
    assert!(message.contains(&file_url(&fixture.engine)), "{message}");
}

#[test]
fn checkout_whose_remote_is_the_games_git_source_compiles_without_a_warning() {
    // The same URL, and the same repository spelled as a plain local path.
    for remote in [
        |engine: &Path| file_url(engine),
        |engine: &Path| slashes(engine),
    ] {
        let fixture = Fixture::new(git_dependency);
        git(
            &fixture.engine,
            &["remote", "add", "origin", &remote(&fixture.engine)],
        );
        let artifact = compiler()
            .compile(&fixture.request(), &AtomicBool::new(false))
            .unwrap();
        assert_single_engine_copy(&fixture, &artifact.directory, FACADE);
        assert_eq!(unverified_warnings(&artifact), 0);
        assert!(
            Command::new(&artifact.executable)
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn spellings_cargo_treats_as_one_source_share_one_patch() {
    let fixture = Fixture::new(|engine| {
        let url = file_url(engine);
        format!(
            "engine={{git=\"{url}\",tag=\"v0.1.0\"}}
engine-core={{git=\"{url}/\",tag=\"v0.1.0\"}}"
        )
    });
    fs::write(
        fixture.game.join("src/lib.rs"),
        "pub fn configure(app: &mut engine::core::EngineApp) -> Result<(),String> { app.value=7; Ok(()) }
",
    )
    .unwrap();
    let artifact = compiler()
        .compile(&fixture.request(), &AtomicBool::new(false))
        .unwrap();
    let manifest = fs::read_to_string(artifact.directory.join("Cargo.toml")).unwrap();
    let sources: std::collections::BTreeSet<_> = manifest
        .lines()
        .filter(|line| line.starts_with("[patch."))
        .map(|line| line.split('"').nth(1).unwrap_or(line))
        .collect();
    assert_eq!(sources.len(), 1, "{manifest}");
    assert!(manifest.contains("engine-core]"), "{manifest}");
    assert_single_engine_copy(&fixture, &artifact.directory, FACADE);
}
