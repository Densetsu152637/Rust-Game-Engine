use engine_assets::*;
use engine_project::{simulation::AnimationGraphPlayer, *};
use glam::{Quat, Vec3};
use serde_json::{Value, json};
use std::fs;
const NODES: [&str; 5] = ["ancestor", "root", "mid", "end", "mesh"];
fn write(path: impl AsRef<std::path::Path>, value: Value) {
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn read(path: impl AsRef<std::path::Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path().join("rig.gltf"),
        json!({"asset":{"version":"2.0"},"nodes":[{"translation":[0.5,0,0],"children":[1]},{"children":[2]},{"translation":[1,0,0],"children":[3]},{"translation":[1,0,0]},{}],"skins":[{"joints":[3]}],"scenes":[{"nodes":[0,4]}],"scene":0}),
    );
    let mut scene = import_gltf(
        dir.path(),
        std::path::Path::new("rig.gltf"),
        &ImportLimits::default(),
    )
    .unwrap();
    scene.meshes.push(ImportedMesh {
        name: None,
        primitives: vec![ImportedPrimitive {
            vertices: [[0., 0., 0.], [0.1, 0., 0.], [0., 0.1, 0.]]
                .into_iter()
                .map(|position| ImportedVertex {
                    position,
                    normal: Some([0., 0., 1.]),
                    tangent: None,
                    uv0: Some([0.25, 0.75]),
                    uv1: None,
                    color: None,
                    joints: Some([0; 4]),
                    weights: Some([1., 0., 0., 0.]),
                })
                .collect(),
            indices: vec![0, 1, 2],
            material: None,
        }],
    });
    scene.nodes[4].mesh = Some(0);
    scene.nodes[4].skin = Some(0);
    let mut entities:Vec<Value>=scene.nodes.iter().enumerate().map(|(i,node)|json!({"id":NODES[i],"name":NODES[i],"parent":node.parent.map(|p|NODES[p]),"components":{"Transform":{"position":node.transform.translation}}})).collect();
    entities.push(json!({"id":"primitive","name":"primitive","parent":"mesh","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"},"custom":{"engine.SkinBinding":{"version":1,"data":{"model":"rig","mesh_node":4,"primitive":0,"bindings":NODES}}}}}));
    entities.push(json!({"id":"player","name":"player","components":{"Transform":{"position":[0,0,0]},"custom":{"engine.AnimationGraphPlayer":{"version":1,"data":{"graph":"graph","rig":"rig","bindings":NODES}}}}}));
    let model = model_import::ModelDocument {
        version: 1,
        scene,
        node_entities: NODES.map(str::to_owned).to_vec(),
    };
    fs::write(
        dir.path().join("model.json"),
        serde_json::to_vec(&model).unwrap(),
    )
    .unwrap();
    write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}),
    );
    write(
        dir.path().join("prefab.json"),
        json!({"schema":1,"scene_id":"prefab","entities":entities}),
    );
    write(
        dir.path().join("clip.json"),
        json!({"version":1,"duration":1,"joints":5,"tracks":[]}),
    );
    write(
        dir.path().join("graph.json"),
        json!({"version":1,"kind":"animation","nodes":[
        {"id":"clip","node_type":"clip","params":{"clip":"clip"}},
        {"id":"ik","node_type":"ik_two_bone","params":{"root":1,"mid":2,"end":3,"target":"target","pole":"pole","weight":"weight"}},
        {"id":"out","node_type":"output"}],"edges":[{"from":{"node":"clip","port":"pose"},"to":{"node":"ik","port":"input"}},{"from":{"node":"ik","port":"pose"},"to":{"node":"out","port":"pose"}}],"variables":[{"name":"target","type":"vec3","default":[1.5,1,0]},{"name":"pole","type":"vec3","default":[0.5,2,0]},{"name":"weight","type":"float","default":1}]}),
    );
    fs::write(
        dir.path().join("triangle.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"ik-runtime","startup_scene":"scene.json","assets":[{"id":"rig","kind":"model","path":"model.json"},{"id":"graph","kind":"graph","path":"graph.json"},{"id":"clip","kind":"animation","path":"clip.json"},{"id":"prefab","kind":"prefab","path":"prefab.json"},{"id":"triangle","kind":"mesh","path":"triangle.cgmesh"}]}),
    );
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    runtime
}
fn vertex(runtime: &Runtime, name: &str) -> Vec3 {
    let entity = runtime.entity(name).unwrap();
    Vec3::from(
        runtime
            .app()
            .world()
            .get::<GeometryOverride>(entity)
            .unwrap()
            .0
            .vertices[0]
            .position,
    )
}
fn position(runtime: &Runtime, name: &str) -> Vec3 {
    let entity = runtime.entity(name).unwrap();
    Vec3::from(
        world_transform(runtime.app().world(), entity)
            .unwrap()
            .position(),
    )
}
fn parameter(runtime: &mut Runtime, name: &str, value: Value) {
    let entity = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<AnimationGraphPlayer>(entity)
        .unwrap()
        .parameters
        .insert(name.into(), value);
}
fn close(a: Vec3, b: Vec3) {
    assert!(a.distance(b) < 4e-4, "{a:?} != {b:?}");
}
fn lengths(runtime: &Runtime) {
    assert!((position(runtime, "mid").distance(position(runtime, "root")) - 1.).abs() < 4e-4);
    assert!((position(runtime, "end").distance(position(runtime, "mid")) - 1.).abs() < 4e-4);
}
#[test]
fn authored_ik_moves_actual_skin_with_target_pole_weight_and_non_joint_ancestor() {
    let dir = fixture();
    let mut runtime = runtime(&dir);
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, 1., 0.));
    close(position(&runtime, "mid"), Vec3::new(0.5, 1., 0.));
    lengths(&runtime);
    parameter(&mut runtime, "pole", json!([0.5, -2, 0]));
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    close(position(&runtime, "mid"), Vec3::new(1.5, 0., 0.));
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, 1., 0.));
    lengths(&runtime);
    parameter(&mut runtime, "target", json!([1.5, -1, 0]));
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, -1., 0.));
    lengths(&runtime);
    parameter(&mut runtime, "weight", json!(0.));
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    close(vertex(&runtime, "primitive"), Vec3::new(2.5, 0., 0.));
    lengths(&runtime);
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id == "mesh:primitive")
        .unwrap();
    close(
        Vec3::from(draw.mesh.vertices[0].position),
        Vec3::new(2.5, 0., 0.),
    );
    assert_eq!(draw.mesh.vertices[0].uv, [0.25, 0.75]);
}
#[test]
fn unreachable_targets_and_authored_elbow_limits_preserve_bone_lengths() {
    let dir = fixture();
    let mut graph = read(dir.path().join("graph.json"));
    graph["nodes"][1]["params"]["min_bend"] = json!(0.3);
    graph["nodes"][1]["params"]["max_bend"] = json!(0.5);
    write(dir.path().join("graph.json"), graph);
    let mut runtime = runtime(&dir);
    for target in [[20., 1., 0.], [0.6, 0.1, 0.]] {
        parameter(&mut runtime, "target", json!(target));
        runtime.tick(0.02, InputSnapshot::default()).unwrap();
        lengths(&runtime);
        let a = (position(&runtime, "mid") - position(&runtime, "root")).normalize();
        let b = (position(&runtime, "end") - position(&runtime, "mid")).normalize();
        let angle = a.dot(b).clamp(-1., 1.).acos();
        assert!((0.3 - 4e-4..=0.5 + 4e-4).contains(&angle), "{angle}");
    }
}
fn anchor(dir: &tempfile::TempDir, scale: [f32; 3], both_roots: bool) {
    let mut scene = read(dir.path().join("scene.json"));
    for entity in scene["entities"].as_array_mut().unwrap() {
        if entity["id"] == "ancestor" || (both_roots && entity["id"] == "mesh") {
            entity["parent"] = json!("anchor");
        }
    }
    scene["entities"].as_array_mut().unwrap().push(json!({"id":"anchor","name":"anchor","components":{"Transform":{"position":[10,0,0],"rotation":Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array(),"scale":scale}}}));
    write(dir.path().join("scene.json"), scene);
}
#[test]
fn common_external_anchor_is_excluded_from_target_coordinates_and_nonuniform_anchor_fails() {
    let dir = fixture();
    anchor(&dir, [2., 2., 2.], true);
    let runtime = runtime(&dir);
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, 1., 0.));
    close(position(&runtime, "end"), Vec3::new(8., 3., 0.));
    let bad = fixture();
    anchor(&bad, [1., 2., 1.], true);
    let mut runtime =
        Runtime::new(Project::load(bad.path().join("project.json")).unwrap()).unwrap();
    assert!(
        runtime
            .start()
            .unwrap_err()
            .to_string()
            .contains("positive uniform scale")
    );
    let split = fixture();
    anchor(&split, [1., 1., 1.], false);
    let mut runtime =
        Runtime::new(Project::load(split.path().join("project.json")).unwrap()).unwrap();
    assert!(
        runtime
            .start()
            .unwrap_err()
            .to_string()
            .contains("share one external anchor")
    );
}
#[test]
fn missing_rig_bindings_or_changed_hierarchy_are_rejected_without_guessing() {
    for fault in ["rig", "bindings", "parent", "index"] {
        let dir = fixture();
        let mut scene = read(dir.path().join("scene.json"));
        let player = scene["entities"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["id"] == "player")
            .unwrap();
        if fault == "rig" {
            player["components"]["custom"]["engine.AnimationGraphPlayer"]["data"]
                .as_object_mut()
                .unwrap()
                .remove("rig");
        }
        if fault == "bindings" {
            player["components"]["custom"]["engine.AnimationGraphPlayer"]["data"]["bindings"]
                .as_array_mut()
                .unwrap()
                .remove(0);
        }
        if fault == "parent" {
            scene["entities"][1]["parent"] = Value::Null;
        }
        if fault == "index" {
            let mut graph = read(dir.path().join("graph.json"));
            graph["nodes"][1]["params"]["end"] = json!(4);
            write(dir.path().join("graph.json"), graph);
        }
        write(dir.path().join("scene.json"), scene);
        let mut runtime =
            Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
        assert!(runtime.start().is_err(), "{fault}");
    }
    let dir = fixture();
    let mut runtime = runtime(&dir);
    let before = vertex(&runtime, "primitive");
    let mid = runtime.entity("mid").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(mid)
        .remove::<Parent>();
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
    close(vertex(&runtime, "primitive"), before);
}
#[test]
fn prefab_full_rig_bindings_remap_and_invalid_controls_preserve_last_skin() {
    let dir = fixture();
    let mut document = SceneDocument::new(Project::load(dir.path().join("project.json")).unwrap());
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "prefab".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &revision,
        )
        .unwrap();
    let player = document
        .snapshot()
        .entities
        .iter()
        .find(|e| e.id == "copy:player")
        .unwrap();
    assert_eq!(
        player.components.custom["engine.AnimationGraphPlayer"].data["rig"],
        "rig"
    );
    assert_eq!(
        player.components.custom["engine.AnimationGraphPlayer"].data["bindings"],
        json!(NODES.map(|id| format!("copy:{id}")))
    );
    let revision = document.revision().to_owned();
    let mut runtime = Runtime::new(document.begin_play(&revision).unwrap()).unwrap();
    runtime.start().unwrap();
    close(vertex(&runtime, "copy:primitive"), Vec3::new(1.5, 1., 0.));
    let before = vertex(&runtime, "primitive");
    parameter(&mut runtime, "weight", json!(2.));
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
    close(vertex(&runtime, "primitive"), before);
    parameter(&mut runtime, "weight", json!(1.));
    parameter(&mut runtime, "pole", json!([2.5, 2, 0]));
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
    close(vertex(&runtime, "primitive"), before);
}
#[test]
fn ik_keeps_ragdoll_order_and_remote_target_authority() {
    use simulation::physics::{Ragdoll, RagdollBone};
    let dir = fixture();
    let mut runtime = runtime(&dir);
    let owner = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(owner)
        .insert(Ragdoll {
            bones: ["root", "mid", "end"]
                .map(|bone| RagdollBone {
                    bone: bone.into(),
                    ..Default::default()
                })
                .to_vec(),
            ..Default::default()
        });
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    parameter(&mut runtime, "target", json!([1.5, -1, 0]));
    runtime.tick(0.001, InputSnapshot::default()).unwrap();
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, -1., 0.));
    let end = runtime.entity("end").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(end)
        .insert(networking::NetworkReplica {
            authority: networking::ReplicaAuthority::Remote,
        });
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
}
#[test]
fn rig_retains_authored_bound_rest_pose_and_rejects_nonuniform_chain_scale() {
    let dir = fixture();
    let mut scene = read(dir.path().join("scene.json"));
    scene["entities"][0]["components"]["Transform"]["position"] = json!([2, 0, 0]);
    write(dir.path().join("scene.json"), scene);
    let runtime = runtime(&dir);
    close(position(&runtime, "ancestor"), Vec3::new(2., 0., 0.));
    close(position(&runtime, "root"), Vec3::new(2., 0., 0.));
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, 1., 0.));
    lengths(&runtime);
    let bad = fixture();
    let mut scene = read(bad.path().join("scene.json"));
    scene["entities"][1]["components"]["Transform"]["scale"] = json!([1, 2, 1]);
    write(bad.path().join("scene.json"), scene);
    let mut runtime =
        Runtime::new(Project::load(bad.path().join("project.json")).unwrap()).unwrap();
    assert!(
        runtime
            .start()
            .unwrap_err()
            .to_string()
            .contains("positive uniform scale")
    );
}
#[test]
fn valid_component_overrides_replace_unusable_default_ik_controls_before_startup() {
    let dir = fixture();
    let mut graph = read(dir.path().join("graph.json"));
    for variable in graph["variables"].as_array_mut().unwrap() {
        if variable["type"] == "vec3" {
            variable.as_object_mut().unwrap().remove("default");
        }
    }
    write(dir.path().join("graph.json"), graph);
    let mut scene = read(dir.path().join("scene.json"));
    let player = scene["entities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["id"] == "player")
        .unwrap();
    player["components"]["custom"]["engine.AnimationGraphPlayer"]["data"]["parameters"] =
        json!({"target":[1.5,1,0],"pole":[0.5,2,0]});
    write(dir.path().join("scene.json"), scene.clone());
    let runtime = runtime(&dir);
    close(vertex(&runtime, "primitive"), Vec3::new(1.5, 1., 0.));
    let player = scene["entities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["id"] == "player")
        .unwrap();
    player["components"]["custom"]["engine.AnimationGraphPlayer"]["data"]["parameters"]["pole"] =
        json!([2.5, 2, 0]);
    write(dir.path().join("scene.json"), scene);
    let mut invalid =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    assert!(
        invalid
            .start()
            .unwrap_err()
            .to_string()
            .contains("collinear")
    );
}
