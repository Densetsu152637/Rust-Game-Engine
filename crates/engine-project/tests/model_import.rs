#[path = "support/model_fixture.rs"]
mod fixture;
use engine_project::{
    EditOperation, Project, SceneDocument, content, load_scene,
    model_import::{import_model, load_model},
};
use std::{fs, path::Path};

#[test]
fn imports_renderable_prefab_and_retains_full_model_metadata() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_source(temp.path(), false);
    let project = Project::load(temp.path().join("project.json")).unwrap();
    let before = fs::read(temp.path().join("main.json")).unwrap();
    let imported = import_model(&project, Path::new("source/model.gltf"), "ship").unwrap();
    assert_eq!(imported.assets.len(), 4);
    assert_eq!(imported.prefab, "ship:prefab");
    let reloaded = Project::load(&project.manifest).unwrap();
    let prefab = load_scene(&reloaded.root, &reloaded.assets["ship:prefab"].path).unwrap();
    assert_eq!(prefab.entities.len(), 4);
    let node = prefab.entities.iter().find(|n| n.id == "node:1").unwrap();
    assert_eq!(node.parent.as_deref(), Some("node:0"));
    assert_eq!(node.components.transform.as_ref().unwrap().scale, [2.; 3]);
    let primitive = prefab
        .entities
        .iter()
        .find(|n| n.id == "node:1:primitive:0")
        .unwrap();
    let material = serde_json::to_value(&primitive.components).unwrap()["MeshMaterial"].clone();
    assert_eq!(material["texture"], "ship:texture:0");
    assert_eq!(material["transparent"], true);
    let mesh = content::load_mesh(&reloaded.resolve_asset("ship:mesh:0:0").unwrap()).unwrap();
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    assert_eq!(mesh.vertices[1].color, [0., 1., 0., 1.]);
    let texture =
        content::load_texture(&reloaded.resolve_asset("ship:texture:0").unwrap()).unwrap();
    assert_eq!(texture.pixels, vec![255, 32, 16, 255]);
    let model = load_model(&reloaded.resolve_asset("ship:model").unwrap()).unwrap();
    assert_eq!(model.node_entities, vec!["node:0", "node:1"]);
    assert_eq!(model.scene.materials[0].metallic, 0.9);
    let mut document = SceneDocument::new(reloaded);
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: imported.prefab,
                prefix: "instance:".into(),
                parent: None,
            },
            &revision,
        )
        .unwrap();
    assert!(
        document
            .snapshot()
            .entities
            .iter()
            .any(|e| e.id == "instance:node:1:primitive:0")
    );
    assert_eq!(fs::read(temp.path().join("main.json")).unwrap(), before);
}
#[test]
fn import_rejects_conflicts_and_escapes_without_manifest_changes() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_source(temp.path(), false);
    let project = Project::load(temp.path().join("project.json")).unwrap();
    let before = fs::read(&project.manifest).unwrap();
    for (source, prefix) in [("../outside.gltf", "test"), ("source/model.gltf", "../bad")] {
        assert!(import_model(&project, Path::new(source), prefix).is_err());
        assert_eq!(fs::read(&project.manifest).unwrap(), before);
    }
    fs::create_dir(temp.path().join("assets")).unwrap();
    fs::create_dir(temp.path().join("assets/imported-conflict")).unwrap();
    fs::write(
        temp.path().join("assets/imported-conflict/keep.txt"),
        "keep",
    )
    .unwrap();
    assert!(import_model(&project, Path::new("source/model.gltf"), "conflict").is_err());
    assert_eq!(fs::read(&project.manifest).unwrap(), before);
    assert_eq!(
        fs::read_to_string(temp.path().join("assets/imported-conflict/keep.txt")).unwrap(),
        "keep"
    );
    import_model(&project, Path::new("source/model.gltf"), "ship").unwrap();
    let reloaded = Project::load(&project.manifest).unwrap();
    let committed = fs::read(&project.manifest).unwrap();
    assert!(import_model(&reloaded, Path::new("source/model.gltf"), "ship").is_err());
    assert_eq!(fs::read(&project.manifest).unwrap(), committed);
    assert!(!fs::read_dir(temp.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".model-import-")
    }));
}
#[test]
fn generated_animation_bindings_remain_valid_after_prefab_instantiation() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_source(temp.path(), true);
    let project = Project::load(temp.path().join("project.json")).unwrap();
    import_model(&project, Path::new("source/model.gltf"), "animated").unwrap();
    let project = Project::load(&project.manifest).unwrap();
    assert_eq!(project.assets["animated:animation:0"].kind, "animation");
    let mut document = SceneDocument::new(project);
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "animated:prefab".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &revision,
        )
        .unwrap();
    let root = document
        .snapshot()
        .entities
        .iter()
        .find(|e| e.id == "copy:root")
        .unwrap();
    assert_eq!(
        root.components.custom["engine.AnimationPlayer"].data["bindings"],
        serde_json::json!(["copy:node:0", "copy:node:1"])
    );
}
#[test]
fn bounded_json_mesh_loader_rejects_invalid_geometry() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("mesh.json");
    fs::write(
        &file,
        r#"{"vertices":[{"position":[0,0,0],"uv":[0,0],"color":[1,1,1,1]}],"indices":[0,1,2]}"#,
    )
    .unwrap();
    assert!(content::load_mesh(&file).is_err());
}
#[test]
fn model_documents_validate_references_before_consumption() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_source(temp.path(), false);
    let scene = engine_assets::import_gltf(
        temp.path(),
        Path::new("source/model.gltf"),
        &engine_assets::ImportLimits::default(),
    )
    .unwrap();
    let mut document = engine_project::model_import::ModelDocument {
        version: 1,
        scene,
        node_entities: vec!["node:0".into(), "node:1".into()],
    };
    document.validate().unwrap();
    document.scene.nodes[0].parent = Some(99999);
    assert!(document.validate().is_err());
    document.scene.nodes[0].parent = None;
    document.scene.meshes[0].primitives[0].indices[0] = 99;
    assert!(document.validate().is_err());
}
#[test]
fn generated_skin_bindings_remap_and_model_rejects_invalid_joint_indices() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_skinned_source(temp.path());
    let project = Project::load(temp.path().join("project.json")).unwrap();
    import_model(&project, Path::new("source/model.gltf"), "skinned").unwrap();
    let project = Project::load(&project.manifest).unwrap();
    let mut model = load_model(&project.resolve_asset("skinned:model").unwrap()).unwrap();
    assert_eq!(model.scene.skins[0].joints, vec![2]);
    model.scene.meshes[0].primitives[0].vertices[0].joints = Some([1, 0, 0, 0]);
    assert!(model.validate().is_err());
    model.scene.meshes[0].primitives[0].vertices[0].joints = Some([0; 4]);
    model.scene.meshes[0].primitives[0].vertices[0].weights = Some([0.; 4]);
    assert!(model.validate().is_err());
    let mut document = SceneDocument::new(project);
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "skinned:prefab".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &revision,
        )
        .unwrap();
    let primitive = document
        .snapshot()
        .entities
        .iter()
        .find(|e| e.id == "copy:node:1:primitive:0")
        .unwrap();
    assert_eq!(
        primitive.components.custom["engine.SkinBinding"].data,
        serde_json::json!({"model":"skinned:model","mesh_node":1,"primitive":0,
            "bindings":["copy:node:0","copy:node:1","copy:node:2"]})
    );
}
