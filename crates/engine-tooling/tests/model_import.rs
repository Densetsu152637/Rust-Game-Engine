#[path = "../../engine-project/tests/support/model_fixture.rs"]
#[allow(dead_code)]
mod fixture;
use std::process::Command;

#[test]
fn cli_model_import_writes_project_assets_without_compiling_game_code() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_source(temp.path(), false);
    let output = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(["project", "import-model"])
        .arg(temp.path().join("project.json"))
        .args(["source/model.gltf", "--prefix", "cli"])
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{result}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(result["result"]["prefab"], "cli:prefab");
    let project = engine_project::Project::load(temp.path().join("project.json")).unwrap();
    let mesh = engine_project::content::load_mesh(&project.resolve_asset("cli:mesh:0:0").unwrap())
        .unwrap();
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    let texture =
        engine_project::content::load_texture(&project.resolve_asset("cli:texture:0").unwrap())
            .unwrap();
    assert_eq!(texture.pixels, vec![255, 32, 16, 255]);
    assert!(!temp.path().join("target").exists());
}
