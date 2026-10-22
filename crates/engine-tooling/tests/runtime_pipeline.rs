//! Real external-Rust generations built against this engine checkout. Dependency
//! output is shared in `target/native-generation-shared` so only the first test
//! compiles the engine's dependency graph. All runs are headless.
use engine_editor::{PlayHost, PlayStatus};
use engine_project::{EditOperation, Project, SceneDocument};
use engine_tooling::play_host::{NativePlayHost, PlayMode};
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn engine_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn shared_target() -> PathBuf {
    engine_root().join("target/native-generation-shared")
}

struct Output {
    code: i32,
    json: Value,
    stderr: String,
}
fn engine<S: AsRef<OsStr>>(args: &[S], cwd: &Path, envs: &[(&str, &str)]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(args)
        .current_dir(cwd)
        .env("ENGINE_NATIVE_TARGET_DIR", shared_target())
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let lines = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "one JSON line expected:\n{stdout}\n{stderr}"
    );
    Output {
        code: output.status.code().unwrap_or(-1),
        json: serde_json::from_str(lines[0]).unwrap(),
        stderr,
    }
}
fn success(output: Output) -> Value {
    assert_eq!(output.code, 0, "{}\n{}", output.json, output.stderr);
    assert_eq!(output.json["ok"], true);
    output.json
}
fn engine_path(name: &str) -> String {
    engine_root()
        .join("crates")
        .join(name)
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

const PLAYER: &str = r#"
    use engine_core::bevy_ecs::prelude::*;
    use engine_project::{NativeScript,Transform,InputSnapshot};
    struct Player;
    impl NativeScript for Player {
        fn on_create(&mut self,world:&mut World,owner:Entity)->engine_project::Result<()> {
            let x=world.get::<Transform>(owner).unwrap().position[0];
            world.resource_mut::<engine_project::runtime::RuntimeLog>().0.push(format!("created at {x}"));
            Ok(())
        }
        fn on_update(&mut self,world:&mut World,owner:Entity,dt:f32)->engine_project::Result<()> {
            if world.resource::<InputSnapshot>().held("move_right") {world.get_mut::<Transform>(owner).unwrap().position[0]+=2.0*dt;}
            std::thread::sleep(std::time::Duration::from_millis(1));
            Ok(())
        }
        fn on_destroy(&mut self,world:&mut World,_:Entity)->engine_project::Result<()> { world.resource_mut::<engine_project::runtime::RuntimeLog>().0.push("clean native teardown".into()); Ok(()) }
    }
    pub fn configure(app:&mut engine_core::EngineApp)->engine_core::Result<()> {
        match std::env::var("PIPELINE_MODE").as_deref() {
            Ok("fail") => return Err(engine_core::anyhow::anyhow!("configure refused by fixture")),
            Ok("panic") => panic!("fixture panic"),
            _ => {}
        }
        engine_project::register_script(app,"asset:player",|_,_|Ok(Box::new(Player)))?;Ok(())
    }
"#;

/// A native game project in a fresh directory under the engine's target dir.
fn game(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(engine_root().join("target"))
        .unwrap();
    let game = temp.path().join("game");
    fs::create_dir_all(game.join("src")).unwrap();
    fs::write(game.join("Cargo.toml"),format!("[package]\nname='pipeline-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nengine-core={{path={:?}}}\nengine-project={{path={:?}}}\n",engine_path("engine-core"),engine_path("engine-project"))).unwrap();
    fs::write(game.join("src/lib.rs"), PLAYER).unwrap();
    let manifest = game.join("project.json");
    fs::write(&manifest,r#"{"schema":1,"name":"native pipeline","startup_scene":"main.scene.json","rust":{"manifest":"Cargo.toml"},"inputActions":{"move_right":"Right"},"assets":[{"id":"asset:player","kind":"script","path":"src/lib.rs"}]}"#).unwrap();
    fs::write(game.join("main.scene.json"),r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"entity:player","name":"Player","components":{"Transform":{"position":[0,0,0]},"Script":{"asset":"asset:player"}}}]}"#).unwrap();
    (temp, manifest)
}

fn generations(manifest: &Path) -> Vec<PathBuf> {
    let cache = manifest.parent().unwrap().join(".derived/native");
    fs::read_dir(cache)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("generation.json").is_file())
        .collect()
}

fn wait_for_play(host: &mut NativePlayHost) -> PlayStatus {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        match host.poll().unwrap() {
            PlayStatus::Compiling => {
                assert!(Instant::now() < deadline, "play compile timed out");
                thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

#[test]
fn cli_compiles_runs_packages_and_editor_play_uses_unsaved_scene_without_saving() {
    let (temp, manifest) = game("pipeline-");
    let game = manifest.parent().unwrap().to_path_buf();
    let authored = fs::read(game.join("main.scene.json")).unwrap();
    let validated = success(engine(
        &[
            "project".as_ref(),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
        &[],
    ));
    assert_eq!(
        validated["result"]["rust"]["diagnostics"],
        serde_json::json!([])
    );
    assert_eq!(
        validated["result"]["rust"]["fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let executed = success(engine(
        &[
            "run".as_ref(),
            manifest.as_os_str(),
            "--headless".as_ref(),
            "--ticks".as_ref(),
            "2".as_ref(),
            "--input".as_ref(),
            "move_right:0".as_ref(),
        ],
        temp.path(),
        &[],
    ));
    assert_eq!(executed["command"], "run");
    let x =
        executed["result"]["scene"]["scene"]["entities"][0]["components"]["Transform"]["position"]
            [0]
        .as_f64()
        .unwrap();
    assert!((x - 2.0 / 60.0).abs() < 0.000001);
    assert_eq!(executed["result"]["presented_frames"], 0);
    assert!(
        executed["result"]["scene"]["logs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "clean native teardown")
    );

    let mut document = SceneDocument::new(Project::load(&manifest).unwrap());
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::SetPosition {
                entity_id: "entity:player".into(),
                position: [7.0, 0.0, 0.0],
            },
            &revision,
        )
        .unwrap();
    let project = document
        .begin_play(document.revision().to_owned().as_str())
        .unwrap();
    // Exercise the exact isolated snapshot adapter headlessly and inspect its
    // returned positions, then the asynchronous compiler-backed PlayHost itself.
    let snapshot = temp.path().join("editor-observed.json");
    fs::write(&snapshot, serde_json::to_vec(&project.scene).unwrap()).unwrap();
    let unsaved = success(engine(
        &[
            "run".as_ref(),
            manifest.as_os_str(),
            "--headless".as_ref(),
            "--ticks".as_ref(),
            "1".as_ref(),
            "--scene-override".as_ref(),
            snapshot.as_os_str(),
        ],
        temp.path(),
        &[],
    ));
    assert_eq!(
        unsaved["result"]["scene"]["scene"]["entities"][0]["components"]["Transform"]["position"]
            [0],
        7.0
    );
    let mut host = NativePlayHost::with_mode(PlayMode::Headless).with_target_dir(shared_target());
    host.start(project).unwrap();
    assert_eq!(wait_for_play(&mut host), PlayStatus::Running);
    host.stop().unwrap();
    assert!(matches!(
        host.poll().unwrap(),
        PlayStatus::Stopped { success: true, .. }
    ));
    assert_eq!(authored, fs::read(game.join("main.scene.json")).unwrap());
    // CLI builds prune old generations; at most three remain.
    assert!(generations(&manifest).len() <= 3);

    let package = temp.path().join("package");
    let packaged_result = success(engine(
        &[
            "project".as_ref(),
            "package".as_ref(),
            manifest.as_os_str(),
            package.as_os_str(),
            "--profile".as_ref(),
            "dev".as_ref(),
        ],
        temp.path(),
        &[],
    ));
    assert_eq!(packaged_result["result"]["profile"], "dev");
    fs::rename(&game, temp.path().join("source-unavailable")).unwrap();
    let executable = package.join(if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    });
    let elsewhere = tempfile::tempdir().unwrap();
    let packaged = Command::new(&executable)
        .arg(package.join("game/project.json"))
        .args(["--headless", "--ticks", "1"])
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stderr)
    );
    let report: Value = serde_json::from_slice(&packaged.stdout).unwrap();
    assert_eq!(report["ok"], true);
    // The launcher script resolves its own directory, not the caller's.
    let launcher = if cfg!(windows) {
        // cmd.exe cannot execute verbatim (`\\?\`) script paths; users launch
        // the script through an ordinary path.
        let script = package.join("run.cmd");
        let script = script.to_string_lossy();
        Command::new("cmd")
            .arg("/C")
            .arg(script.trim_start_matches(r"\\?\"))
            .args(["--headless", "--ticks", "1"])
            .current_dir(elsewhere.path())
            .output()
            .unwrap()
    } else {
        Command::new(package.join("run.sh"))
            .args(["--headless", "--ticks", "1"])
            .current_dir(elsewhere.path())
            .output()
            .unwrap()
    };
    assert!(
        launcher.status.success(),
        "{}",
        String::from_utf8_lossy(&launcher.stderr)
    );
    let report: Value = serde_json::from_slice(&launcher.stdout).unwrap();
    assert_eq!(report["ok"], true);
    let notices: Value =
        serde_json::from_slice(&fs::read(package.join("licenses/dependencies.json")).unwrap())
            .unwrap();
    assert!(
        notices
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "bevy_ecs")
    );
    assert!(package.join("licenses/engine-LICENSE").is_file());
    assert!(package.join("game/src/lib.rs").is_file());
    assert!(!package.join("game/.derived").exists());
}

#[test]
fn invalid_external_build_reports_diagnostics_and_keeps_last_good_generation() {
    let (temp, manifest) = game("pipeline-invalid-");
    success(engine(
        &[
            "run".as_ref(),
            manifest.as_os_str(),
            "--headless".as_ref(),
            "--ticks".as_ref(),
            "1".as_ref(),
        ],
        temp.path(),
        &[],
    ));
    let good = generations(&manifest);
    assert_eq!(good.len(), 1);
    let source = manifest.parent().unwrap().join("src/lib.rs");
    fs::write(
        &source,
        format!("{PLAYER}\npub fn broken() -> u32 {{ \"not a number\" }}\n"),
    )
    .unwrap();

    for command in [
        vec![
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        vec![
            OsStr::new("run"),
            manifest.as_os_str(),
            "--headless".as_ref(),
        ],
    ] {
        let failed = engine(&command, temp.path(), &[]);
        assert_eq!(failed.code, 1, "{command:?}: {}", failed.json);
        assert_eq!(failed.json["ok"], false);
        let diagnostic = &failed.json["result"]["diagnostics"][0];
        assert_eq!(diagnostic["code"], "rust.E0308", "{}", failed.json);
        assert_eq!(diagnostic["severity"], "error");
        assert!(diagnostic["file"].as_str().unwrap().ends_with("lib.rs"));
        assert!(diagnostic["path"].as_str().unwrap().contains(':'));
        assert!(failed.stderr.contains("E0308"));
    }
    let cache = manifest.parent().unwrap().join(".derived/native");
    assert!(
        fs::read_dir(&cache)
            .unwrap()
            .filter_map(|e| e.ok())
            .all(|e| !e.file_name().to_string_lossy().starts_with("check-"))
    );
    // The last good generation is intact and still runs.
    assert_eq!(generations(&manifest), good);
    let runtime = good[0].join(if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    });
    let rerun = Command::new(runtime)
        .arg(&manifest)
        .args(["--headless", "--ticks", "1"])
        .output()
        .unwrap();
    assert!(rerun.status.success());
    let report: Value = serde_json::from_slice(&rerun.stdout).unwrap();
    assert_eq!(report["ok"], true);

    // Editor play reports the same build failure and ends the session.
    let project = Project::load(&manifest).unwrap();
    let mut host = NativePlayHost::with_mode(PlayMode::Headless).with_target_dir(shared_target());
    host.start(project.clone()).unwrap();
    match wait_for_play(&mut host) {
        PlayStatus::Stopped { success, message } => {
            assert!(!success);
            assert!(message.contains("E0308"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    // Stopping during compilation cancels it without error.
    host.start(project).unwrap();
    host.stop().unwrap();
    assert!(matches!(
        host.poll().unwrap(),
        PlayStatus::Stopped { success: true, .. }
    ));
}

#[test]
fn native_configure_errors_and_panics_are_reported_as_run_json() {
    let (temp, manifest) = game("pipeline-runtime-");
    let args = [
        OsStr::new("run"),
        manifest.as_os_str(),
        "--headless".as_ref(),
        "--ticks".as_ref(),
        "1".as_ref(),
    ];
    let failed = engine(&args, temp.path(), &[("PIPELINE_MODE", "fail")]);
    assert_eq!(failed.code, 3, "{}", failed.json);
    assert_eq!(failed.json["command"], "run");
    assert_eq!(
        failed.json["result"]["diagnostics"][0]["code"],
        "runtime.failed"
    );
    assert!(
        failed.json["result"]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("configure refused by fixture")
    );
    let panicked = engine(&args, temp.path(), &[("PIPELINE_MODE", "panic")]);
    assert_eq!(panicked.code, 3, "{}", panicked.json);
    assert_eq!(
        panicked.json["result"]["diagnostics"][0]["code"],
        "runtime.panic"
    );
    assert!(panicked.stderr.contains("fixture panic"));
}
