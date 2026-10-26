use engine_assets::*;
use engine_project::{simulation::AnimationGraphPlayer, *};
use serde_json::{Value, json};
use std::fs;
fn write(path: impl AsRef<std::path::Path>, value: Value) {
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn edge(source: &str, target: &str, port: &str) -> Value {
    json!({"from":{"node":source,"port":"pose"},"to":{"node":target,"port":port}})
}
fn graph() -> Value {
    json!({"version":1,"kind":"animation","nodes":[
    {"id":"a","node_type":"clip","params":{"clip":"a"}},
    {"id":"b","node_type":"clip","params":{"clip":"b"}},
    {"id":"mix","node_type":"blend1d","params":{"parameter":"weight","positions":[0,1]}},
    {"id":"out","node_type":"output"}],"edges":[edge("a","mix","samples"),edge("b","mix","samples"),edge("mix","out","pose")],"variables":[{"name":"weight","type":"float","default":0}]})
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path().join("rig.gltf"),
        json!({"asset":{"version":"2.0"},"nodes":[{},{}],"skins":[{"joints":[0]}],"scenes":[{"nodes":[0,1]}],"scene":0}),
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
            vertices: [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]
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
    scene.nodes[1].mesh = Some(0);
    scene.nodes[1].skin = Some(0);
    let model = model_import::ModelDocument {
        version: 1,
        scene,
        node_entities: vec!["bone".into(), "mesh".into()],
    };
    fs::write(
        dir.path().join("model.json"),
        serde_json::to_vec(&model).unwrap(),
    )
    .unwrap();
    let entities = json!([
        {"id":"bone","name":"bone","components":{"Transform":{"position":[0,0,0]}}},
        {"id":"mesh","name":"mesh","components":{"Transform":{"position":[0,0,0]}}},
        {"id":"primitive","name":"primitive","parent":"mesh","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"},"custom":{"engine.SkinBinding":{"version":1,"data":{"model":"rig","mesh_node":1,"primitive":0,"bindings":["bone","mesh"]}}}}},
        {"id":"player","name":"player","components":{"Transform":{"position":[0,0,0]},"custom":{"engine.AnimationGraphPlayer":{"version":1,"data":{"graph":"graph","bindings":["bone","mesh"],"parameters":{"weight":0.25}}}}}}
    ]);
    write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}),
    );
    write(
        dir.path().join("prefab.json"),
        json!({"schema":1,"scene_id":"prefab","entities":entities}),
    );
    write(dir.path().join("graph.json"), graph());
    for (id, value) in [("a", 0.), ("b", 10.)] {
        write(
            dir.path().join(format!("{id}.json")),
            json!({"version":1,"duration":1,"joints":2,"tracks":[{"joint":0,"translation":{"times":[0,1],"values":[[value,0,0],[value,0,0]]}}]}),
        );
    }
    fs::write(
        dir.path().join("triangle.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"graph-runtime","startup_scene":"scene.json","assets":[{"id":"rig","kind":"model","path":"model.json"},{"id":"graph","kind":"graph","path":"graph.json"},{"id":"a","kind":"animation","path":"a.json"},{"id":"b","kind":"animation","path":"b.json"},{"id":"prefab","kind":"prefab","path":"prefab.json"},{"id":"triangle","kind":"mesh","path":"triangle.cgmesh"}]}),
    );
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    runtime
}
fn vertex(runtime: &Runtime) -> f32 {
    let id = runtime.entity("primitive").unwrap();
    runtime
        .app()
        .world()
        .get::<GeometryOverride>(id)
        .unwrap()
        .0
        .vertices[0]
        .position[0]
}
fn weight(runtime: &mut Runtime, value: f32) {
    let id = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<AnimationGraphPlayer>(id)
        .unwrap()
        .parameters
        .insert("weight".into(), json!(value));
}
#[test]
fn authored_graph_blend_deforms_real_skin_and_parameter_changes_keep_clip_state() {
    let dir = fixture();
    let mut runtime = runtime(&dir);
    assert!((vertex(&runtime) - 2.5).abs() < 1e-4);
    weight(&mut runtime, 0.75);
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert!((vertex(&runtime) - 7.5).abs() < 1e-4);
    let bone = runtime.entity("bone").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(bone)
            .unwrap()
            .position,
        [7.5, 0., 0.]
    );
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id == "mesh:primitive")
        .unwrap();
    assert!((draw.mesh.vertices[0].position[0] - 7.5).abs() < 1e-4);
    let primitive = runtime.entity("primitive").unwrap();
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<GeometryOverride>(primitive)
            .unwrap()
            .0
            .vertices[0]
            .uv,
        [0.25, 0.75]
    );
}
#[test]
fn graph_assets_validate_clips_semantics_and_parameter_types_before_pose_changes() {
    let dir = fixture();
    let mut bad = graph();
    bad["nodes"][2]["params"]["positions"] = json!([0, 0]);
    write(dir.path().join("graph.json"), bad);
    assert!(Project::load(dir.path().join("project.json")).is_err());
    let mut bad = graph();
    bad["nodes"][0]["params"]["clip"] = json!("missing");
    write(dir.path().join("graph.json"), bad);
    assert!(Project::load(dir.path().join("project.json")).is_err());
    write(dir.path().join("graph.json"), graph());
    let mut runtime = runtime(&dir);
    let player = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<AnimationGraphPlayer>(player)
        .unwrap()
        .parameters
        .insert("weight".into(), json!(true));
    assert!(runtime.tick(0.02, InputSnapshot::default()).is_err());
    assert!((vertex(&runtime) - 2.5).abs() < 1e-4);
}
#[test]
fn graph_pose_authority_rejects_clip_physics_and_other_graph_writers() {
    let dir = fixture();
    let mut first = runtime(&dir);
    let player = first.entity("player").unwrap();
    first
        .app_mut()
        .world_mut()
        .entity_mut(player)
        .insert(simulation::AnimationPlayer {
            clip: "a".into(),
            bindings: vec!["bone".into(), "mesh".into()],
            ..Default::default()
        });
    assert!(first.tick(0.02, InputSnapshot::default()).is_err());
    let mut second = runtime(&dir);
    let bone = second.entity("bone").unwrap();
    second
        .app_mut()
        .world_mut()
        .entity_mut(bone)
        .insert(simulation::PhysicsBody3D::default());
    assert!(second.tick(0.02, InputSnapshot::default()).is_err());
    let mut third = runtime(&dir);
    let mesh = third.entity("mesh").unwrap();
    third
        .app_mut()
        .world_mut()
        .entity_mut(mesh)
        .insert(AnimationGraphPlayer {
            graph: "graph".into(),
            bindings: vec!["bone".into(), "mesh".into()],
            ..Default::default()
        });
    assert!(third.tick(0.02, InputSnapshot::default()).is_err());
}
#[test]
fn prefab_graph_bindings_are_remapped_and_both_instances_deform_independently() {
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
        player.components.custom["engine.AnimationGraphPlayer"].data["bindings"],
        json!(["copy:bone", "copy:mesh"])
    );
    let revision = document.revision().to_owned();
    let mut runtime = Runtime::new(document.begin_play(&revision).unwrap()).unwrap();
    runtime.start().unwrap();
    let id = runtime.entity("copy:primitive").unwrap();
    assert!(
        (runtime
            .app()
            .world()
            .get::<GeometryOverride>(id)
            .unwrap()
            .0
            .vertices[0]
            .position[0]
            - 2.5)
            .abs()
            < 1e-4
    );
}
#[test]
fn animated_ragdoll_consumes_graph_pose_before_skinning_including_substep_frames() {
    let dir = fixture();
    let mut runtime = runtime(&dir);
    let owner = runtime.entity("player").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(owner)
        .insert(simulation::physics::Ragdoll {
            bones: vec![simulation::physics::RagdollBone {
                bone: "bone".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
    weight(&mut runtime, 0.8);
    runtime.tick(0.02, InputSnapshot::default()).unwrap();
    assert!((vertex(&runtime) - 8.).abs() < 1e-3);
    weight(&mut runtime, 0.6);
    runtime.tick(0.001, InputSnapshot::default()).unwrap();
    assert!((vertex(&runtime) - 6.).abs() < 1e-3);
}
#[test]
fn changing_parameters_preserves_continuous_graph_clip_clocks() {
    let dir = fixture();
    for (id, value) in [("a", 0.), ("b", 10.)] {
        write(
            dir.path().join(format!("{id}.json")),
            json!({"version":1,"duration":1,"joints":2,"tracks":[{"joint":0,"translation":{"times":[0,1],"values":[[value,0,0],[value+2.,0,0]]}}]}),
        );
    }
    let mut runtime = runtime(&dir);
    runtime.tick(0.2, InputSnapshot::default()).unwrap();
    assert!((vertex(&runtime) - 2.9).abs() < 1e-4);
    weight(&mut runtime, 0.75);
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    assert!((vertex(&runtime) - 8.1).abs() < 1e-4);
}
#[test]
fn remote_network_replicas_cannot_own_graphs_or_be_graph_targets() {
    use engine_project::networking::{NetworkReplica, ReplicaAuthority};
    let dir = fixture();
    for name in ["player", "bone"] {
        let mut runtime = runtime(&dir);
        let entity = runtime.entity(name).unwrap();
        runtime
            .app_mut()
            .world_mut()
            .entity_mut(entity)
            .insert(NetworkReplica {
                authority: ReplicaAuthority::Remote,
            });
        let error = runtime.tick(0.02, InputSnapshot::default()).unwrap_err();
        assert!(
            error.to_string().contains("Remote replica")
                || error.to_string().contains("remote network pose authority"),
            "{error}"
        );
        assert!((vertex(&runtime) - 2.5).abs() < 1e-4);
    }
}
