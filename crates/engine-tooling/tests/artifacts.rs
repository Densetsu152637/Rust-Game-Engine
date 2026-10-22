use engine_project::Project;
use engine_tooling::{arguments::parse_run, artifacts::*, compiler::BuildArtifact};
use std::{fs, path::Path};

fn fixture(root: &Path) -> Project {
    fs::create_dir(root).unwrap();
    fs::write(root.join("project.json"),r#"{"schema":1,"name":"tooling fixture","startup_scene":"main.scene.json","assets":[{"id":"asset:color","kind":"texture","path":"color.ppm"}]}"#).unwrap();
    fs::write(
        root.join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
    )
    .unwrap();
    fs::write(root.join("color.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    Project::load(root.join("project.json")).unwrap()
}

#[test]
fn index_tracks_stable_id_rename_and_content_changes_and_replaces_cache() {
    let temp = tempfile::tempdir().unwrap();
    let mut project = fixture(&temp.path().join("game"));
    let first = asset_index(&project).unwrap();
    let cache = write_asset_index(&project).unwrap();
    write_asset_index(&project).unwrap();
    let saved: AssetIndex = serde_json::from_slice(&fs::read(cache).unwrap()).unwrap();
    assert_eq!(first, saved);
    fs::rename(
        project.root.join("color.ppm"),
        project.root.join("renamed.ppm"),
    )
    .unwrap();
    project.assets.get_mut("asset:color").unwrap().path = "renamed.ppm".into();
    let moved = asset_index(&project).unwrap();
    assert_eq!(first.assets[0].id, moved.assets[0].id);
    assert_eq!(first.assets[0].content_hash, moved.assets[0].content_hash);
    fs::write(project.root.join("renamed.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    assert_ne!(
        moved.assets[0].content_hash,
        asset_index(&project).unwrap().assets[0].content_hash
    );
}

#[test]
fn initialize_and_package_refuse_existing_destination_and_leave_authored_data() {
    let temp = tempfile::tempdir().unwrap();
    let project = fixture(&temp.path().join("game"));
    let original = fs::read(&project.manifest).unwrap();
    write_asset_index(&project).unwrap();
    let initialized = temp.path().join("new-game");
    initialize_project(&project.manifest, &initialized).unwrap();
    assert!(!initialized.join(".derived").exists());
    assert!(initialize_project(&project.manifest, &initialized).is_err());
    let generation = temp.path().join("generation");
    fs::create_dir(&generation).unwrap();
    fs::write(generation.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(generation.join("cargo-metadata.json"), r#"{"packages":[]}"#).unwrap();
    let engine = temp.path().join("engine");
    fs::create_dir(&engine).unwrap();
    fs::write(engine.join("LICENSE"), "fixture license").unwrap();
    let artifact = BuildArtifact {
        fingerprint: "test".into(),
        generation: "test".into(),
        executable: std::env::current_exe().unwrap(),
        directory: generation,
        diagnostics: vec![],
    };
    let output = temp.path().join("package");
    package_project(&project, &artifact, &output, &engine).unwrap();
    Project::load(output.join("game/project.json")).unwrap();
    assert!(!output.join("game/.derived").exists());
    assert!(
        fs::read_to_string(output.join("run.cmd"))
            .unwrap()
            .contains("%~dp0")
    );
    assert!(output.join("licenses/engine-LICENSE").is_file());
    assert!(output.join("licenses/runtime-Cargo.lock").is_file());
    assert!(package_project(&project, &artifact, &output, &engine).is_err());
    assert_eq!(original, fs::read(project.manifest).unwrap());
}

#[test]
fn run_parser_preserves_namespaced_actions_and_rejects_invalid_runs() {
    let values = [
        "game/project.json",
        "--headless",
        "--ticks",
        "3",
        "--input",
        "ui:confirm:1",
        "--input",
        "move_right:1",
        "--format",
        "json",
    ]
    .map(Into::into);
    let args = parse_run(&values).unwrap();
    assert!(args.headless);
    assert_eq!(args.ticks, 3);
    assert!(args.inputs[&1].contains("ui:confirm"));
    assert!(args.inputs[&1].contains("move_right"));
    for invalid in [
        vec!["project.json", "--ticks", "0"],
        vec!["project.json", "--ticks", "2", "--input", "right:2"],
        vec!["project.json", "--headless", "--headless"],
        vec!["project.json", "--unknown", "value"],
    ] {
        assert!(parse_run(&invalid.into_iter().map(Into::into).collect::<Vec<_>>()).is_err());
    }
}
