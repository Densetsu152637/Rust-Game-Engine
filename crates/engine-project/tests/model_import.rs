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
    assert!((material["metallic"].as_f64().unwrap() - 0.9).abs() < 1e-5);
    assert!((material["roughness"].as_f64().unwrap() - 0.2).abs() < 1e-5);
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
#[test]
fn importer_maps_pbr_textures_and_source_normals_into_render_assets() {
    use serde_json::json;
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_source(temp.path(), false);
    let file = temp.path().join("source/model.gltf");
    let mut source: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let data = temp.path().join("source/geometry.bin");
    let mut bytes = fs::read(&data).unwrap();
    let offset = bytes.len();
    for _ in 0..3 {
        for v in [0.0f32, 0.0, 1.0] {
            bytes.extend(v.to_le_bytes());
        }
    }
    source["buffers"][0]["byteLength"] = json!(bytes.len());
    source["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(json!({"buffer":0,"byteOffset":offset,"byteLength":36}));
    source["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView":5,"componentType":5126,"count":3,"type":"VEC3"}));
    source["meshes"][0]["primitives"][0]["attributes"]["NORMAL"] = json!(5);
    source["materials"][0]["pbrMetallicRoughness"]["metallicRoughnessTexture"] = json!({"index":0});
    source["materials"][0]["normalTexture"] = json!({"index":0,"scale":0.75});
    fs::write(&file, serde_json::to_vec(&source).unwrap()).unwrap();
    fs::write(data, bytes).unwrap();
    let project = Project::load(temp.path().join("project.json")).unwrap();
    import_model(&project, Path::new("source/model.gltf"), "pbr").unwrap();
    let project = Project::load(&project.manifest).unwrap();
    let prefab = load_scene(&project.root, &project.assets["pbr:prefab"].path).unwrap();
    let material = prefab
        .entities
        .iter()
        .find_map(|e| e.components.material.as_ref())
        .unwrap();
    assert_eq!(material.normal_texture.as_deref(), Some("pbr:texture:0"));
    assert_eq!(
        material.metallic_roughness_texture.as_deref(),
        Some("pbr:texture:0")
    );
    assert_eq!(material.normal_scale, 0.75);
    let mesh = content::load_mesh(&project.resolve_asset("pbr:mesh:0:0").unwrap()).unwrap();
    assert_eq!(mesh.normals, vec![[0.0, 0.0, 1.0]; 3]);
    // Shared-UV renderer rejects incompatible per-map coordinate sets at import.
    source["materials"][0]["normalTexture"]["texCoord"] = json!(1);
    fs::write(&file, serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(
        import_model(&project, Path::new("source/model.gltf"), "mixed")
            .unwrap_err()
            .to_string()
            .contains("UV set")
    );
}
#[test]
fn mesh_loader_rejects_mismatched_normal_array() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("mesh.json");
    let vertices = vec![serde_json::json!({"position":[0,0,0],"uv":[0,0],"color":[1,1,1,1]}); 3];
    fs::write(
        &path,
        serde_json::to_vec(
            &serde_json::json!({"vertices":vertices,"indices":[0,1,2],"normals":[[0,0,1]]}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(content::load_mesh(&path).is_err());
}

#[test]
fn morph_import_builds_live_prefab_controls_and_keeps_bindings_during_instantiation() {
    let temp = tempfile::tempdir().unwrap();
    fixture::write_project(temp.path());
    fixture::write_morph_source(temp.path());
    let project = Project::load(temp.path().join("project.json")).unwrap();
    import_model(&project, Path::new("source/model.gltf"), "morph").unwrap();
    let project = Project::load(&project.manifest).unwrap();
    let mut model = load_model(&project.resolve_asset("morph:model").unwrap()).unwrap();
    assert_eq!(model.scene.morphs.node_weights[&1], vec![0.25]);
    assert!(matches!(
        model.scene.clips[0].channels[0].values,
        engine_assets::ChannelValues::Weights(_)
    ));
    model.scene.morphs.primitives[0].weights.push(0.);
    assert!(model.validate().is_err());
    let mut document = SceneDocument::new(project);
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "morph:prefab".into(),
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
        primitive.components.custom["engine.MorphWeights"].data,
        serde_json::json!({"model":"morph:model","mesh_node":1,"primitive":0,"weights":[]})
    );
    assert_eq!(
        primitive.components.custom["engine.MorphPlayer"].data["clip"],
        0
    );
}
