use engine_tooling::compiler::{CompileRequest, GenerationManager, NativeCompiler};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    request: CompileRequest,
}
impl Fixture {
    fn new() -> Self {
        // Kept short: Cargo and rustc nest long build-script and object file
        // names below this root, and the MSVC linker rejects paths past
        // MAX_PATH (LNK1181) when the checkout itself is deeply nested.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tooling-fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        // A crashed earlier run with a reused process ID may have left it behind.
        let _ = fs::remove_dir_all(&root);
        let engine = root.join("engine");
        let game = root.join("game");
        write(
            &engine.join("crates/engine-core/Cargo.toml"),
            "[package]\nname='engine-core'\nversion='0.1.0'\nedition='2024'\n[workspace]\n",
        );
        write(
            &engine.join("crates/engine-core/src/lib.rs"),
            "#[derive(Default)] pub struct EngineApp { pub value: i32 }\n",
        );
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='external-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[features]\nalternate=[]\n[dependencies]\nengine-core={path='../engine/crates/engine-core'}\n",
        );
        write(
            &game.join("src/lib.rs"),
            "pub fn configure(app: &mut engine_core::EngineApp) -> Result<(),String> { app.value=7; Ok(()) }\n",
        );
        let request = CompileRequest {
            engine_root: engine, project_manifest: game.join("Cargo.toml"), cache_root: root.join("cache"),
            main_source: "fn main() { let mut app=engine_core::EngineApp::default(); gameplay::configure(&mut app).unwrap(); assert_eq!(app.value,7); }".into(),
            features: vec![], target: None, release: false, offline: true, target_dir: None,
        };
        Self { root, request }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn write(path: &Path, source: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}
fn compiler() -> NativeCompiler {
    let cargo = PathBuf::from(env!("CARGO"));
    let rustc = cargo
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    NativeCompiler::new(cargo, rustc)
}
fn uncancelled() -> AtomicBool {
    AtomicBool::new(false)
}
const COOPERATIVE_LOOP: &str = "fn main() { let stop=std::env::var_os(\"ENGINE_GENERATION_STOP_FILE\").unwrap(); while !std::path::Path::new(&stop).exists() { std::thread::sleep(std::time::Duration::from_millis(10)); } }";

#[test]
fn compiles_real_external_rust_and_preserves_last_good_on_diagnostics() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let first = compiler.compile(&fixture.request, &uncancelled()).unwrap();
    assert!(
        compiler
            .compile(&fixture.request, &AtomicBool::new(true))
            .unwrap_err()
            .cancelled
    );
    assert_eq!(compiler.last_good().unwrap().generation, first.generation);
    assert!(
        std::process::Command::new(&first.executable)
            .status()
            .unwrap()
            .success()
    );
    let source = fixture
        .request
        .project_manifest
        .parent()
        .unwrap()
        .join("src/lib.rs");
    write(
        &source,
        "pub fn configure(_: &mut engine_core::EngineApp) -> Result<(),String> { let invalid: u32 = \"bad\"; Ok(()) }",
    );
    let failed = compiler
        .compile(&fixture.request, &uncancelled())
        .unwrap_err();
    assert_eq!(failed.code, "compiler.build");
    assert!(
        failed
            .diagnostics
            .iter()
            .any(|d| d["level"] == "error" && d["code"]["code"] == "E0308"),
        "{failed:?}"
    );
    assert_eq!(compiler.last_good().unwrap().generation, first.generation);
    assert!(
        std::process::Command::new(&first.executable)
            .status()
            .unwrap()
            .success()
    );
    write(
        &source,
        "pub fn configure(app: &mut engine_core::EngineApp) -> Result<(),String> { app.value=9; Ok(()) }",
    );
    let mut revised = fixture.request.clone();
    revised.main_source = revised.main_source.replace("app.value,7", "app.value,9");
    let second = compiler.compile(&revised, &uncancelled()).unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
    assert_ne!(first.executable, second.executable);
    assert!(
        std::process::Command::new(&second.executable)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new(&first.executable)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn generation_hash_tracks_dirty_engine_lock_features_and_ignores_cache() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let mut request = fixture.request.clone();
    request.cache_root = request
        .project_manifest
        .parent()
        .unwrap()
        .join("native-cache");
    let first = compiler.compile(&request, &uncancelled()).unwrap();
    let same = compiler.compile(&request, &uncancelled()).unwrap();
    assert_eq!(first.fingerprint, same.fingerprint);
    assert_ne!(first.generation, same.generation);
    write(
        &request.engine_root.join("crates/engine-core/src/lib.rs"),
        "#[derive(Default)] pub struct EngineApp { pub value:i32 } // dirty source\n",
    );
    let dirty = compiler.compile(&request, &uncancelled()).unwrap();
    assert_ne!(same.fingerprint, dirty.fingerprint);
    write(
        &request.engine_root.join("Cargo.lock"),
        "# seeded lockfile\nversion = 4\n",
    );
    let lock = compiler.compile(&request, &uncancelled()).unwrap();
    assert_ne!(dirty.fingerprint, lock.fingerprint);
    request.features.push("alternate".into());
    let feature = compiler.compile(&request, &uncancelled()).unwrap();
    assert_ne!(lock.fingerprint, feature.fingerprint);
}

#[test]
fn failed_candidate_compiles_before_previous_child_is_stopped() {
    let fixture = Fixture::new();
    let mut request = fixture.request.clone();
    request.main_source = COOPERATIVE_LOOP.into();
    let mut manager = GenerationManager::new(compiler());
    manager
        .rebuild_and_replace(&request, &[], &uncancelled())
        .unwrap();
    let old_id = manager.active.as_ref().unwrap().id();
    request.main_source = "fn main() { impossible Rust syntax; }".into();
    assert!(
        manager
            .rebuild_and_replace(&request, &[], &uncancelled())
            .is_err()
    );
    assert_eq!(manager.active.as_ref().unwrap().id(), old_id);
    assert!(
        manager
            .active
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
    );
    request.main_source = "fn main() { std::process::exit(7); }".into();
    manager
        .rebuild_and_replace(&request, &[], &uncancelled())
        .unwrap();
    assert_ne!(manager.active.as_ref().unwrap().id(), old_id);
    let exit = manager
        .active
        .as_mut()
        .unwrap()
        .wait(&uncancelled())
        .unwrap();
    assert!(!exit.success);
    assert_eq!(exit.code, Some(7));
}

#[test]
fn cancellation_and_missing_compiler_are_explicit_failures() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let cancel = AtomicBool::new(true);
    let failure = compiler.compile(&fixture.request, &cancel).unwrap_err();
    assert_eq!(failure.code, "compiler.cancelled");
    assert!(failure.cancelled);
    compiler.cargo = fixture.root.join("missing-cargo");
    assert_eq!(
        compiler
            .compile(&fixture.request, &uncancelled())
            .unwrap_err()
            .code,
        "compiler.io"
    );
}

#[test]
fn cancelling_a_live_generation_reaps_it() {
    let fixture = Fixture::new();
    let mut request = fixture.request.clone();
    request.main_source = COOPERATIVE_LOOP.into();
    let mut manager = GenerationManager::new(compiler());
    let child = manager
        .rebuild_and_replace(&request, &[], &uncancelled())
        .unwrap();
    let exit = child.wait(&AtomicBool::new(true)).unwrap();
    assert!(exit.cancelled);
    assert!(exit.success);
    assert!(!exit.forced);
    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn readiness_gate_precedes_gameplay_and_stop_allows_native_cleanup() {
    let fixture = Fixture::new();
    let marker = fixture.root.join("lifecycle.txt");
    let mut request = fixture.request.clone();
    request.main_source = r#"fn main() {
        let marker=std::env::args_os().nth(1).unwrap();
        std::fs::write(&marker,b"started").unwrap();
        let stop=std::env::var_os("ENGINE_GENERATION_STOP_FILE").unwrap();
        while !std::path::Path::new(&stop).exists() { std::thread::sleep(std::time::Duration::from_millis(5)); }
        std::fs::write(&marker,b"clean shutdown").unwrap();
    }"#.into();
    let artifact = compiler().compile(&request, &uncancelled()).unwrap();
    let mut child = engine_tooling::compiler::RunningGeneration::prepare(
        artifact,
        &[marker.clone().into_os_string()],
        &uncancelled(),
    )
    .unwrap();
    assert!(!marker.exists());
    child.start().unwrap();
    // Generous: only reaching the marker matters, and a loaded CI runner may be
    // slow to schedule the freshly started child. The loop exits on success.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(marker.exists());
    let exit = child.stop().unwrap();
    assert!(exit.success && exit.cancelled && !exit.forced);
    assert_eq!(fs::read_to_string(marker).unwrap(), "clean shutdown");
}

#[test]
fn in_flight_cargo_cancellation_terminates_build_script_tree_and_retains_last_good() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let good = compiler.compile(&fixture.request, &uncancelled()).unwrap();
    let game = fixture.request.project_manifest.parent().unwrap();
    write(
        &game.join("build.rs"),
        r#"fn main() {
        std::fs::write("build-started",std::process::id().to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
        std::fs::write("build-finished",b"orphan").unwrap();
    }"#,
    );
    let cancel = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let child_cancel = cancel.clone();
        let request = &fixture.request;
        let compiler_ref = &mut compiler;
        let build = scope.spawn(move || compiler_ref.compile(request, &child_cancel));
        // Cargo must first compile and link the build script; allow for a
        // loaded CI runner. The loop exits as soon as the script starts.
        let deadline = Instant::now() + Duration::from_secs(120);
        while !game.join("build-started").exists()
            && !build.is_finished()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        if !game.join("build-started").exists() {
            let outcome = build.is_finished().then(|| build.join().unwrap());
            panic!("build script never started; build outcome: {outcome:?}");
        }
        let cancellation_start = Instant::now();
        cancel.store(true, Ordering::Release);
        let result = build.join().unwrap().unwrap_err();
        assert!(result.cancelled);
        // Far below the build script's 60-second sleep, so cancellation must
        // have terminated the tree rather than waited for it.
        assert!(cancellation_start.elapsed() < Duration::from_secs(20));
    });
    assert_eq!(compiler.last_good().unwrap().generation, good.generation);
    assert!(!game.join("build-finished").exists());
}

#[test]
fn external_game_uses_real_public_ecs_components_bundles_and_system_sets() {
    let fixture = Fixture::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let core = root
        .join("crates/engine-core")
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    write(
        &fixture.request.project_manifest,
        &format!(
            "[package]\nname='external-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nengine-core={{path={core:?}}}\n"
        ),
    );
    write(
        &fixture
            .request
            .project_manifest
            .parent()
            .unwrap()
            .join("src/lib.rs"),
        r#"
        use engine_core::{prelude::*, bevy_ecs};
        #[derive(Component)] struct Arbitrary { name: String, values: std::collections::HashMap<String,Vec<u64>> }
        #[derive(Bundle)] struct NativeBundle { arbitrary: Arbitrary }
        #[derive(SystemSet,Clone,Debug,Hash,Eq,PartialEq)] enum NativeSet { Mutate,Check }
        fn mutate(mut q: Query<&mut Arbitrary>) { for mut item in &mut q { item.name.push_str(" native"); item.values.insert("unrestricted".into(),vec![1,2,3]); } }
        fn check(q: Query<&Arbitrary>) { for item in &q { assert_eq!(item.name,"external native"); assert_eq!(item.values["unrestricted"],vec![1,2,3]); } }
        pub fn configure(app:&mut EngineApp)->Result<()> {
            app.world_mut().spawn(NativeBundle { arbitrary:Arbitrary { name:"external".into(),values:Default::default() } });
            app.schedule_mut(Stage::Update).configure_sets((NativeSet::Mutate,NativeSet::Check).chain())
                .add_systems((mutate.in_set(NativeSet::Mutate),check.in_set(NativeSet::Check)));
            Ok(())
        }
    "#,
    );
    let mut request = fixture.request.clone();
    request.engine_root = root.clone();
    request.cache_root = root.join("target/native-core-generations");
    request.main_source="fn main(){ let mut app=engine_core::EngineApp::new(); gameplay::configure(&mut app).unwrap(); app.startup().unwrap(); app.tick(1.0/60.0).unwrap(); app.shutdown().unwrap(); }".into();
    let artifact = compiler().compile(&request, &uncancelled()).unwrap();
    assert!(
        std::process::Command::new(artifact.executable)
            .status()
            .unwrap()
            .success()
    );
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

/// A game crate nested in an unrelated outer directory, built into a cache under
/// the game exactly as the CLI does (`<project>/.derived/native`).
fn nested_game(fixture: &Fixture, outer_manifest: &str, game_package: &str) -> CompileRequest {
    let outer = fixture.root.join("outer");
    write(&outer.join("Cargo.toml"), outer_manifest);
    let game = outer.join("game");
    let core = toml_path(&fixture.request.engine_root.join("crates/engine-core"));
    write(
        &game.join("Cargo.toml"),
        &format!(
            "[package]\nname='external-game'\n{game_package}\n[dependencies]\nengine-core={{path={core:?}}}\n"
        ),
    );
    write(
        &game.join("src/lib.rs"),
        "#[cfg(ancestor_leak)] compile_error!(\"ancestor cargo config leaked\");\npub fn configure(app: &mut engine_core::EngineApp) -> Result<(),String> { app.value=7; Ok(()) }\n",
    );
    let mut request = fixture.request.clone();
    request.project_manifest = game.join("Cargo.toml");
    request.cache_root = game.join(".derived/native");
    request.main_source = format!(
        "#[cfg(ancestor_leak)] compile_error!(\"leak\");\n{}",
        request.main_source
    );
    request
}

#[test]
fn ancestor_cargo_config_and_unrelated_workspace_do_not_leak_into_generation() {
    let fixture = Fixture::new();
    let request = nested_game(
        &fixture,
        "[workspace]\nmembers=[]\n",
        "version='0.1.0'\nedition='2024'",
    );
    let outer = fixture.root.join("outer");
    write(
        &outer.join(".cargo/config.toml"),
        "[build]\nrustflags=['--cfg','ancestor_leak']\n",
    );
    write(
        &outer.join("game/.cargo/config.toml"),
        "[build]\nrustflags=['--cfg','ancestor_leak']\n",
    );
    let mut compiler = compiler();
    let first = compiler.compile(&request, &uncancelled()).unwrap();
    assert!(
        std::process::Command::new(&first.executable)
            .status()
            .unwrap()
            .success()
    );
    write(
        &outer.join(".cargo/config.toml"),
        "[build]\nrustflags=['--cfg','ancestor_leak','--cfg','changed']\n",
    );
    write(
        &outer.join("Cargo.toml"),
        "[workspace]\nmembers=[]\n# unrelated edit\n",
    );
    let second = compiler.compile(&request, &uncancelled()).unwrap();
    assert_eq!(first.fingerprint, second.fingerprint);
}

#[test]
fn inherited_workspace_manifest_is_a_generation_input() {
    let fixture = Fixture::new();
    let workspace =
        "[workspace]\nmembers=['game']\n[workspace.package]\nversion='0.1.0'\nedition='2024'\n";
    let request = nested_game(
        &fixture,
        workspace,
        "version.workspace=true\nedition.workspace=true",
    );
    let mut compiler = compiler();
    let first = compiler.compile(&request, &uncancelled()).unwrap();
    write(
        &fixture.root.join("outer/Cargo.toml"),
        &format!("{workspace}description='inherited change'\n"),
    );
    let second = compiler.compile(&request, &uncancelled()).unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
}

#[test]
fn compile_only_check_reports_diagnostics_and_never_replaces_last_good() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let good = compiler.compile(&fixture.request, &uncancelled()).unwrap();
    let source = fixture
        .request
        .project_manifest
        .parent()
        .unwrap()
        .join("src/lib.rs");
    write(
        &source,
        "pub fn configure(_: &mut engine_core::EngineApp) -> Result<(),String> { let invalid: u32 = \"bad\"; Ok(()) }",
    );
    let failed = compiler
        .check(&fixture.request, &uncancelled())
        .unwrap_err();
    assert_eq!(failed.code, "compiler.check");
    assert!(
        failed
            .diagnostics
            .iter()
            .any(|d| d["code"]["code"] == "E0308")
    );
    assert_eq!(compiler.last_good().unwrap().generation, good.generation);
    write(
        &source,
        "pub fn configure(app: &mut engine_core::EngineApp) -> Result<(),String> { let unused = 1; app.value=7; Ok(()) }",
    );
    let report = compiler.check(&fixture.request, &uncancelled()).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d["level"] == "warning" && d["message"].as_str().unwrap().contains("unused"))
    );
    assert_eq!(compiler.last_good().unwrap().generation, good.generation);
    let leftovers = fs::read_dir(&fixture.request.cache_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("check-"))
        .count();
    assert_eq!(leftovers, 0);
    assert!(good.executable.is_file());
}

#[test]
fn shared_target_directory_holds_dependency_output_outside_the_cache() {
    let fixture = Fixture::new();
    let mut request = fixture.request.clone();
    request.target_dir = Some(fixture.root.join("shared-target"));
    let artifact = compiler().compile(&request, &uncancelled()).unwrap();
    assert!(fixture.root.join("shared-target/debug").is_dir());
    assert!(!request.cache_root.join("target").exists());
    assert!(
        artifact
            .executable
            .starts_with(request.cache_root.canonicalize().unwrap())
    );
}

#[test]
fn unresolvable_candidate_leaves_no_generation_directory() {
    let fixture = Fixture::new();
    let mut compiler = compiler();
    let good = compiler.compile(&fixture.request, &uncancelled()).unwrap();
    write(
        &fixture.request.project_manifest,
        "[package]\nname='external-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nabsent-crate={path='../does-not-exist'}\n",
    );
    let failed = compiler
        .compile(&fixture.request, &uncancelled())
        .unwrap_err();
    assert_eq!(failed.code, "compiler.resolve");
    let generations = fs::read_dir(&fixture.request.cache_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != "target")
        .count();
    assert_eq!(generations, 1);
    assert_eq!(compiler.last_good().unwrap().generation, good.generation);
}
