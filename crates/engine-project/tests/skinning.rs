use bevy_ecs::prelude::*;
use engine_assets::*;
use engine_project::{
    GeometryOverride, InputSnapshot, Project, Runtime, model_import::ModelDocument,
};
use serde_json::{Value, json};
fn fixture(bad_joint: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rig.gltf"),r#"{"asset":{"version":"2.0"},"nodes":[{"translation":[1,0,0],"children":[1]},{"translation":[2,0,0],"children":[2]},{"translation":[3,0,0]},{"translation":[10,0,0]}],"skins":[{"joints":[2]}],"scenes":[{"nodes":[0,3]}],"scene":0}"#).unwrap();
    let mut scene = import_gltf(
        dir.path(),
        std::path::Path::new("rig.gltf"),
        &ImportLimits::default(),
    )
    .unwrap();
    let vertices = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]
        .into_iter()
        .map(|position| ImportedVertex {
            position,
            normal: Some([0., 0., 1.]),
            tangent: None,
            uv0: Some([0.25, 0.75]),
            uv1: None,
            color: Some([0.2, 0.4, 0.6, 1.]),
            joints: Some([u16::from(bad_joint), 0, 0, 0]),
            weights: Some([1., 0., 0., 0.]),
        })
        .collect();
    scene.meshes.push(ImportedMesh {
        name: None,
        primitives: vec![ImportedPrimitive {
            vertices,
            indices: vec![0, 1, 2],
            material: None,
        }],
    });
    scene.nodes[3].mesh = Some(0);
    scene.nodes[3].skin = Some(0);
    let node_entities: Vec<_> = (0..4).map(|i| format!("node{i}")).collect();
    let mut entities:Vec<Value>=scene.nodes.iter().enumerate().map(|(i,node)|json!({"id":node_entities[i],"name":node_entities[i],"parent":node.parent.map(|p|node_entities[p].clone()),"components":{"Transform":{"position":node.transform.translation}}})).collect();
    entities.push(json!({"id":"primitive","name":"primitive","parent":"node3","components":{"Transform":{"position":[0,0,0]},"MeshRenderer":{"mesh":"triangle"},"custom":{"engine.SkinBinding":{"version":1,"data":{"model":"rig","mesh_node":3,"primitive":0,"bindings":node_entities}}}}}));
    entities.push(json!({"id":"player","name":"player","components":{"Transform":{"position":[0,0,0]},"custom":{"engine.AnimationPlayer":{"version":1,"data":{"clip":"clip","looping":false,"bindings":node_entities}}}}}));
    let model = ModelDocument {
        version: 1,
        scene,
        node_entities,
    };
    std::fs::write(
        dir.path().join("model.json"),
        serde_json::to_vec(&model).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("clip.json"),serde_json::to_vec(&json!({"version":1,"duration":1.,"joints":4,"tracks":[{"joint":1,"translation":{"times":[0.,1.],"values":[[2.,0.,0.],[4.,0.,0.]]}}]})).unwrap()).unwrap();
    std::fs::write(
        dir.path().join("triangle.cgmesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"skin-runtime","startup_scene":"scene.json","assets":[{"id":"rig","kind":"model","path":"model.json"},{"id":"clip","kind":"animation","path":"clip.json"},{"id":"triangle","kind":"mesh","path":"triangle.cgmesh"}]})).unwrap()).unwrap();
    dir
}
#[derive(Resource, Default)]
struct Extracted(f32);
#[test]
fn runtime_deforms_mesh_before_extraction_preserving_uv_and_color() {
    let dir = fixture(false);
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.app_mut().world_mut().init_resource::<Extracted>();
    runtime
        .app_mut()
        .schedule_mut(engine_core::Stage::Extract)
        .add_systems(
            |meshes: Query<&GeometryOverride>, mut extracted: ResMut<Extracted>| {
                if let Some(mesh) = meshes.iter().next() {
                    extracted.0 = mesh.0.vertices[0].position[0];
                }
            },
        );
    runtime.start().unwrap();
    let primitive = runtime.entity("primitive").unwrap();
    assert!(
        (runtime
            .app()
            .world()
            .get::<GeometryOverride>(primitive)
            .unwrap()
            .0
            .vertices[0]
            .position[0]
            + 4.)
            .abs()
            < 0.001
    );
    runtime.tick(0.5, InputSnapshot::default()).unwrap();
    let mesh = &runtime
        .app()
        .world()
        .get::<GeometryOverride>(primitive)
        .unwrap()
        .0;
    assert!((mesh.vertices[0].position[0] + 3.).abs() < 0.001);
    assert_eq!(mesh.vertices[0].uv, [0.25, 0.75]);
    assert_eq!(mesh.vertices[0].color, [0.2, 0.4, 0.6, 1.]);
    assert!((runtime.app().world().resource::<Extracted>().0 + 3.).abs() < 0.001);
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(primitive)
        .remove::<engine_project::simulation::SkinBinding>();
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
    assert!(
        runtime
            .app()
            .world()
            .get::<GeometryOverride>(primitive)
            .is_none()
    );
}
#[test]
fn edited_model_palette_index_is_rejected_before_render() {
    let dir = fixture(true);
    let project = Project::load(dir.path().join("project.json"));
    if let Ok(project) = project {
        assert!(Runtime::new(project).unwrap().start().is_err());
    }
}
