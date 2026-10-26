use bevy_ecs::prelude::*;
use engine_assets::*;
use engine_project::{simulation::AnimationGraphPlayer, *};
use glam::{Quat, Vec3};
use serde_json::{Value, json};
use std::fs;
const NODES: [&str; 4] = ["root", "mid", "end", "mesh"];
fn write(path: impl AsRef<std::path::Path>, value: Value) {
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn read(path: impl AsRef<std::path::Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn edge(a: &str, b: &str, port: &str) -> Value {
    json!({"from":{"node":a,"port":"pose"},"to":{"node":b,"port":port}})
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path().join("source.gltf"),
        json!({"asset":{"version":"2.0"},"nodes":[{"children":[1]},{"translation":[1,0,0]}],"scenes":[{"nodes":[0]}],"scene":0}),
    );
    let source = import_gltf(
        dir.path(),
        std::path::Path::new("source.gltf"),
        &ImportLimits::default(),
    )
    .unwrap();
    let model = model_import::ModelDocument {
        version: 1,
        scene: source,
        node_entities: vec!["source-root".into(), "source-bone".into()],
    };
    fs::write(
        dir.path().join("source.json"),
        serde_json::to_vec(&model).unwrap(),
    )
    .unwrap();
    write(
        dir.path().join("target.gltf"),
        json!({"asset":{"version":"2.0"},"nodes":[{"children":[1]},{"translation":[2,0,0],"children":[2]},{"translation":[2,0,0]},{}],"skins":[{"joints":[2]}],"scenes":[{"nodes":[0,3]}],"scene":0}),
    );
    let mut target = import_gltf(
        dir.path(),
        std::path::Path::new("target.gltf"),
        &ImportLimits::default(),
    )
    .unwrap();
    target.meshes.push(ImportedMesh {
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
    target.nodes[3].mesh = Some(0);
    target.nodes[3].skin = Some(0);
    let mut entities:Vec<Value>=target.nodes.iter().enumerate().map(|(i,node)|json!({"id":NODES[i],"name":NODES[i],"parent":node.parent.map(|p|NODES[p]),"components":{"Transform":{"position":node.transform.translation}}})).collect();
    entities.push(json!({"id":"primitive","name":"primitive","parent":"mesh","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"},"custom":{"engine.SkinBinding":{"version":1,"data":{"model":"rig","mesh_node":3,"primitive":0,"bindings":NODES}}}}}));
    entities.push(json!({"id":"player","name":"player","components":{"Transform":{"position":[0,0,0]},"EventBindings":{"bindings":[{"event":"animation.event","handler":"record"}]},"custom":{"engine.AnimationGraphPlayer":{"version":1,"data":{"graph":"graph","rig":"rig","bindings":NODES}}}}}));
    let model = model_import::ModelDocument {
        version: 1,
        scene: target,
        node_entities: NODES.map(str::to_owned).to_vec(),
    };
    fs::write(
        dir.path().join("target.json"),
        serde_json::to_vec(&model).unwrap(),
    )
    .unwrap();
    write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}),
    );
    write(
        dir.path().join("foreign.json"),
        json!({"version":1,"duration":1,"joints":2,"tracks":[{"joint":0,"rotation":{"times":[0,1],"values":[Quat::IDENTITY.to_array(),Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array()]}}],"events":[{"time":0.25,"name":"foot"}]}),
    );
    write(
        dir.path().join("rest.json"),
        json!({"version":1,"duration":1,"joints":4,"tracks":[]}),
    );
    write(
        dir.path().join("graph.json"),
        json!({"version":1,"kind":"animation","nodes":[{"id":"retarget","node_type":"retarget_clip","params":{"clip":"foreign","source_rig":"source","target_rig":"rig","mapping":{"joints":[{"source":0,"target":0},{"source":1,"target":1}]}}},{"id":"blend","node_type":"blend","params":{"parameter":"weight"}},{"id":"out","node_type":"output"}],"edges":[edge("retarget","blend","a"),edge("retarget","blend","b"),edge("blend","out","pose")],"variables":[{"name":"weight","type":"float","default":0.5}]}),
    );
    fs::write(
        dir.path().join("triangle.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"retarget-runtime","startup_scene":"scene.json","assets":[{"id":"source","kind":"model","path":"source.json"},{"id":"rig","kind":"model","path":"target.json"},{"id":"foreign","kind":"animation","path":"foreign.json"},{"id":"rest","kind":"animation","path":"rest.json"},{"id":"graph","kind":"graph","path":"graph.json"},{"id":"triangle","kind":"mesh","path":"triangle.cgmesh"}]}),
    );
    dir
}
#[derive(Resource, Default)]
struct Calls(Vec<Value>);
fn record(world: &mut World, _: Entity, event: &EngineEvent) -> engine_project::Result<()> {
    world.resource_mut::<Calls>().0.push(event.payload.clone());
    Ok(())
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.app_mut().world_mut().init_resource::<Calls>();
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    runtime.start().unwrap();
    runtime
}
fn vertex(runtime: &Runtime) -> Vec3 {
    let entity = runtime.entity("primitive").unwrap();
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
#[test]
fn cross_rig_clip_deforms_real_target_skin_and_shared_event_contribution_is_unique() {
    let dir = fixture();
    let mut runtime = runtime(&dir);
    assert!(runtime.entity("source-root").is_none());
    runtime.tick(0.25, InputSnapshot::default()).unwrap();
    let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2 * 0.25) * (Vec3::X * 4.);
    assert!(vertex(&runtime).distance(expected) < 4e-4);
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|draw| draw.id == "mesh:primitive")
        .unwrap();
    assert!(Vec3::from(draw.mesh.vertices[0].position).distance(expected) < 4e-4);
    runtime.tick(0.01, InputSnapshot::default()).unwrap();
    let events = &runtime.app().world().resource::<Calls>().0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["clip"], "foreign");
    assert_eq!(events[0]["name"], "foot");
}
#[test]
fn retarget_branch_can_blend_out_events_without_stopping_its_clock() {
    let dir = fixture();
    let mut graph = read(dir.path().join("graph.json"));
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"rest","node_type":"clip","params":{"clip":"rest"}}));
    graph["edges"][1] = edge("rest", "blend", "b");
    graph["variables"][0]["default"] = json!(1.);
    write(dir.path().join("graph.json"), graph);
    let mut runtime = runtime(&dir);
    runtime.tick(0.25, InputSnapshot::default()).unwrap();
    runtime.tick(0.01, InputSnapshot::default()).unwrap();
    assert!(runtime.app().world().resource::<Calls>().0.is_empty());
    assert!(vertex(&runtime).distance(Vec3::X * 4.) < 4e-4);
    let player = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<AnimationGraphPlayer>(player)
        .unwrap()
        .parameters
        .insert("weight".into(), json!(0.));
    runtime.tick(0.24, InputSnapshot::default()).unwrap();
    let expected = Quat::from_rotation_z(std::f32::consts::FRAC_PI_4) * (Vec3::X * 4.);
    assert!(vertex(&runtime).distance(expected) < 4e-4);
}
#[test]
fn retarget_pose_supports_downstream_ik_and_retains_target_authority() {
    let dir = fixture();
    let mut graph = read(dir.path().join("graph.json"));
    graph["nodes"].as_array_mut().unwrap().push(json!({"id":"ik","node_type":"ik_two_bone","params":{"root":0,"mid":1,"end":2,"target":"target","pole":"pole","weight":"ik_weight"}}));
    graph["edges"][2] = edge("blend", "ik", "input");
    graph["edges"]
        .as_array_mut()
        .unwrap()
        .push(edge("ik", "out", "pose"));
    graph["variables"].as_array_mut().unwrap().extend([
        json!({"name":"target","type":"vec3","default":[2,2,0]}),
        json!({"name":"pole","type":"vec3","default":[0,3,0]}),
        json!({"name":"ik_weight","type":"float","default":1}),
    ]);
    write(dir.path().join("graph.json"), graph);
    let mut runtime = runtime(&dir);
    runtime.tick(0.25, InputSnapshot::default()).unwrap();
    assert!(vertex(&runtime).distance(Vec3::new(2., 2., 0.)) < 4e-4);
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
fn project_validation_resolves_both_rigs_clip_sizes_and_map_topology() {
    for fault in ["missing_rig", "source_count", "map_index", "target_id"] {
        let dir = fixture();
        let mut graph = read(dir.path().join("graph.json"));
        match fault {
            "missing_rig" => graph["nodes"][0]["params"]["source_rig"] = json!("absent"),
            "source_count" => {
                let mut clip = read(dir.path().join("foreign.json"));
                clip["joints"] = json!(3);
                write(dir.path().join("foreign.json"), clip);
            }
            "map_index" => graph["nodes"][0]["params"]["mapping"]["joints"][1]["source"] = json!(9),
            "target_id" => graph["nodes"][0]["params"]["target_rig"] = json!("absent"),
            _ => unreachable!(),
        }
        write(dir.path().join("graph.json"), graph);
        assert!(
            Project::load(dir.path().join("project.json")).is_err(),
            "{fault}"
        );
    }
}
#[test]
fn component_target_rig_label_must_match_the_graph_target_even_when_assets_are_valid() {
    let dir = fixture();
    let mut graph = read(dir.path().join("graph.json"));
    graph["nodes"][0]["params"]["target_rig"] = json!("source");
    write(dir.path().join("graph.json"), graph);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.app_mut().world_mut().init_resource::<Calls>();
    register_event_handler(runtime.app_mut(), "record", record).unwrap();
    assert!(
        runtime
            .start()
            .unwrap_err()
            .to_string()
            .contains("target rig differs")
    );
}
