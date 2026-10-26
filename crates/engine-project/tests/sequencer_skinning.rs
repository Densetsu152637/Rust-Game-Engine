//! These cases exercise the runtime ordering, not a detached timeline/schema mock.
use engine_assets::*;
use engine_project::{
    sequencer::{self, Command, SeekEvents},
    *,
};
use serde_json::{Value, json};
use std::{fs, path::Path};
fn write(path: impl AsRef<Path>, value: Value) {
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn fixture(animated: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path().join("rig.gltf"),
        json!({"asset":{"version":"2.0"},"nodes":[{},{}],"skins":[{"joints":[0]}],"scenes":[{"nodes":[0,1]}],"scene":0}),
    );
    let mut scene =
        import_gltf(dir.path(), Path::new("rig.gltf"), &ImportLimits::default()).unwrap();
    scene.meshes.push(ImportedMesh {
        name: None,
        primitives: vec![ImportedPrimitive {
            vertices: [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]
                .into_iter()
                .map(|position| ImportedVertex {
                    position,
                    normal: Some([0., 0., 1.]),
                    tangent: None,
                    uv0: Some([0., 0.]),
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
    fs::write(
        dir.path().join("triangle.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    let target = if animated { "anchor" } else { "bone" };
    let mut entities = vec![
        json!({"id":"anchor","name":"Anchor","components":{"Transform":{"position":[0,0,0]}}}),
        json!({"id":"bone","name":"Bone","parent":"anchor","components":{"Transform":{"position":[0,0,0]}}}),
        json!({"id":"mesh","name":"Mesh","parent":"anchor","components":{"Transform":{"position":[0,0,0]}}}),
        json!({"id":"primitive","name":"Primitive","parent":"mesh","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"},"custom":{"engine.SkinBinding":{"version":1,"data":{"model":"rig","mesh_node":1,"primitive":0,"bindings":["bone","mesh"]}}}}}),
        json!({"id":"director","name":"Director","components":{"custom":{"engine.SequencePlayer":{"version":1,"data":{"bindings":[target],"timeline":{"version":1,"duration":2,"tracks":[{"binding":0,"property":"translation","channel":{"keys":[{"time":0,"value":[0,0,0]},{"time":2,"value":[4,0,0]}]}}]}}}}}}),
    ];
    if animated {
        entities.push(json!({"id":"animator","name":"Animator","components":{"custom":{"engine.AnimationGraphPlayer":{"version":1,"data":{"graph":"graph","bindings":["bone","mesh"]}}}}}));
    }
    write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"scene","entities":entities}),
    );
    write(
        dir.path().join("clip.json"),
        json!({"version":1,"duration":2,"joints":2,"tracks":[{"joint":0,"translation":{"times":[0,2],"values":[[2,0,0],[4,0,0]]}}]}),
    );
    write(
        dir.path().join("graph.json"),
        json!({"version":1,"kind":"animation","nodes":[{"id":"clip","node_type":"clip","params":{"clip":"clip"}},{"id":"out","node_type":"output"}],"edges":[{"from":{"node":"clip","port":"pose"},"to":{"node":"out","port":"pose"}}]}),
    );
    write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"sequenced-skin","startup_scene":"scene.json","assets":[{"id":"rig","kind":"model","path":"model.json"},{"id":"triangle","kind":"mesh","path":"triangle.cgmesh"},{"id":"clip","kind":"animation","path":"clip.json"},{"id":"graph","kind":"graph","path":"graph.json"}]}),
    );
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    runtime
}
fn local_vertex_x(runtime: &Runtime) -> f32 {
    let primitive = runtime.entity("primitive").unwrap();
    runtime
        .app()
        .world()
        .get::<GeometryOverride>(primitive)
        .unwrap()
        .0
        .vertices[0]
        .position[0]
}
fn world_vertex_x(runtime: &mut Runtime) -> f32 {
    let frame = runtime.extract_scene_frame().unwrap();
    let mesh = &frame.meshes[0];
    let local = mesh.mesh.vertices[0].position;
    glam::Mat4::from_cols_array_2d(&mesh.model)
        .transform_point3(glam::Vec3::from(local))
        .x
}
#[test]
fn sequenced_unanimated_skin_joint_deforms_geometry_during_the_same_tick() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    assert_eq!(local_vertex_x(&rt), 0.);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    assert!((local_vertex_x(&rt) - 1.).abs() < 1e-5);
    assert!((world_vertex_x(&mut rt) - 1.).abs() < 1e-5);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    assert!((local_vertex_x(&rt) - 2.).abs() < 1e-5);
}
#[test]
fn common_animated_rig_anchor_composes_with_current_tick_graph_skinning() {
    let dir = fixture(true);
    let mut rt = runtime(&dir);
    assert!((local_vertex_x(&rt) - 2.).abs() < 1e-5);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    assert!((local_vertex_x(&rt) - 2.5).abs() < 1e-5);
    assert!((world_vertex_x(&mut rt) - 3.5).abs() < 1e-5);
    // Scrubbing refreshes real deformation without advancing graph/physics clocks.
    let owner = rt.entity("director").unwrap();
    sequencer::command(
        rt.app_mut().world_mut(),
        owner,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Silent,
        },
    )
    .unwrap();
    sequencer::command(rt.app_mut().world_mut(), owner, Command::Pause).unwrap();
    assert!((local_vertex_x(&rt) - 2.5).abs() < 1e-5);
    assert!((world_vertex_x(&mut rt) - 5.5).abs() < 1e-5);
}
#[test]
fn direct_graph_pose_target_still_conflicts_while_its_anchor_is_legal() {
    let dir = fixture(true);
    let mut rt = runtime(&dir);
    let owner = rt.entity("director").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<sequencer::SequencePlayer>(owner)
        .unwrap()
        .bindings = vec!["bone".into()];
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
}
#[test]
fn native_joint_seek_and_stop_refresh_deformation_without_a_runtime_tick() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    let owner = rt.entity("director").unwrap();
    sequencer::command(
        rt.app_mut().world_mut(),
        owner,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Silent,
        },
    )
    .unwrap();
    assert!((local_vertex_x(&rt) - 3.).abs() < 1e-5);
    assert!((world_vertex_x(&mut rt) - 3.).abs() < 1e-5);
    sequencer::command(rt.app_mut().world_mut(), owner, Command::Stop).unwrap();
    assert!(local_vertex_x(&rt).abs() < 1e-5);
    assert!(world_vertex_x(&mut rt).abs() < 1e-5);
}
#[test]
fn native_scrub_preserves_graph_morph_and_physics_clocks() {
    let dir = fixture(true);
    let path = dir.path().join("model.json");
    let mut model: model_import::ModelDocument =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    model.scene.morphs.primitives.push(ImportedMorphPrimitive {
        mesh: 0,
        primitive: 0,
        weights: vec![0.25],
        targets: vec![ImportedMorphTarget {
            positions: Some(vec![[2., 0., 0.]; 3]),
            normals: None,
            tangents: None,
        }],
    });
    model.scene.clips.push(ImportedClip {
        name: None,
        duration: 2.,
        channels: vec![ImportedChannel {
            target_node: 1,
            interpolation: Interpolation::Linear,
            times: vec![0., 2.],
            values: ChannelValues::Weights(vec![0., 1.]),
        }],
    });
    fs::write(path, serde_json::to_vec(&model).unwrap()).unwrap();
    let path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let custom = &mut scene["entities"][3]["components"]["custom"];
    custom["engine.MorphWeights"] =
        json!({"version":1,"data":{"model":"rig","mesh_node":1,"primitive":0}});
    custom["engine.MorphPlayer"] = json!({"version":1,"data":{"time":0.5}});
    scene["entities"].as_array_mut().unwrap().push(json!({"id":"body","name":"Body","components":{"Transform":{"position":[20,10,0]},"custom":{"engine.PhysicsBody3D":{"version":1,"data":{}}}}}));
    fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let mut rt = runtime(&dir);
    let primitive = rt.entity("primitive").unwrap();
    let body = rt.entity("body").unwrap();
    let bone = rt.entity("bone").unwrap();
    let owner = rt.entity("director").unwrap();
    let before = simulation::physics::body_state(rt.app().world(), body).unwrap();
    let graph_pose = rt.app().world().get::<Transform>(bone).unwrap().clone();
    let morph_time = rt
        .app()
        .world()
        .get::<simulation::MorphPlayer>(primitive)
        .unwrap()
        .time;
    assert!((local_vertex_x(&rt) - 2.5).abs() < 1e-5);
    sequencer::command(
        rt.app_mut().world_mut(),
        owner,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Silent,
        },
    )
    .unwrap();
    assert!((world_vertex_x(&mut rt) - 5.5).abs() < 1e-5);
    assert_eq!(
        simulation::physics::body_state(rt.app().world(), body).unwrap(),
        before
    );
    assert_eq!(
        rt.app().world().get::<Transform>(bone).unwrap(),
        &graph_pose
    );
    assert_eq!(
        rt.app()
            .world()
            .get::<simulation::MorphPlayer>(primitive)
            .unwrap()
            .time,
        morph_time
    );
}
