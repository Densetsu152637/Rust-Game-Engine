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
    let mesh = runtime
        .app()
        .world()
        .get::<GeometryOverride>(primitive)
        .unwrap()
        .0
        .clone();
    assert!((mesh.vertices[0].position[0] + 3.).abs() < 0.001);
    assert_eq!(mesh.vertices[0].uv, [0.25, 0.75]);
    assert_eq!(mesh.normals, vec![[0.0, 0.0, 1.0]; 3]);
    assert_eq!(mesh.vertices[0].color, [0.2, 0.4, 0.6, 1.]);
    assert!((runtime.app().world().resource::<Extracted>().0 + 3.).abs() < 0.001);
    let frame2d = runtime.extract_frame().unwrap();
    let draw2d = frame2d
        .draws
        .iter()
        .find(|draw| draw.id == "mesh:primitive")
        .unwrap();
    // The 2D path bakes the mesh-node translation into the deformed vertices.
    assert!((draw2d.mesh.vertices[0].position[0] - 7.).abs() < 0.001);
    assert_eq!(draw2d.mesh.vertices[0].uv, [0.25, 0.75]);
    runtime.app_mut().world_mut().spawn((
        engine_project::rendering3d::Camera3D::default(),
        engine_project::Transform::default(),
    ));
    let frame3d = runtime.extract_scene_frame().unwrap();
    let draw3d = frame3d
        .meshes
        .iter()
        .find(|draw| draw.id == "primitive")
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&draw3d.mesh, &mesh));
    assert!((draw3d.mesh.vertices[0].position[0] + 3.).abs() < 0.001);
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

fn morph_runtime(skinned: bool) -> (tempfile::TempDir, Runtime) {
    let dir = fixture(false);
    let path = dir.path().join("model.json");
    let mut model: ModelDocument = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    model.scene.morphs.primitives.push(ImportedMorphPrimitive {
        mesh: 0,
        primitive: 0,
        weights: vec![0.25],
        targets: vec![ImportedMorphTarget {
            positions: Some(vec![[2., 0., 0.]; 3]),
            normals: Some(vec![[0., 1., 0.]; 3]),
            tangents: None,
        }],
    });
    model.scene.morphs.node_weights.insert(3, vec![0.75]);
    model.scene.clips.push(ImportedClip {
        name: None,
        duration: 1.,
        channels: vec![ImportedChannel {
            target_node: 3,
            interpolation: Interpolation::Linear,
            times: vec![0., 1.],
            values: ChannelValues::Weights(vec![-1., 2.]),
        }],
    });
    std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
    let path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    scene["entities"].as_array_mut().unwrap().pop(); // Remove the unrelated TRS player.
    let custom = &mut scene["entities"][4]["components"]["custom"];
    if !skinned {
        custom.as_object_mut().unwrap().remove("engine.SkinBinding");
    }
    custom["engine.MorphWeights"] =
        json!({"version":1,"data":{"model":"rig","mesh_node":3,"primitive":0,"weights":[]}});
    std::fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}
fn morph_position(runtime: &Runtime) -> [f32; 3] {
    runtime
        .app()
        .world()
        .get::<GeometryOverride>(runtime.entity("primitive").unwrap())
        .unwrap()
        .0
        .vertices[0]
        .position
}
#[test]
fn morph_defaults_and_live_controls_deform_before_skin_without_accumulation() {
    use engine_project::simulation::{self, EvaluatedMorphWeights, MorphWeights};
    for skinned in [false, true] {
        let (_dir, mut runtime) = morph_runtime(skinned);
        let offset = if skinned { -4. } else { 0. };
        assert!((morph_position(&runtime)[0] - (offset + 1.5)).abs() < 1e-5);
        let entity = runtime.entity("primitive").unwrap();
        runtime
            .app_mut()
            .world_mut()
            .get_mut::<MorphWeights>(entity)
            .unwrap()
            .weights = vec![-1.];
        simulation::prepare(runtime.app_mut().world_mut()).unwrap();
        assert!((morph_position(&runtime)[0] - (offset - 2.)).abs() < 1e-5);
        simulation::prepare(runtime.app_mut().world_mut()).unwrap();
        assert!((morph_position(&runtime)[0] - (offset - 2.)).abs() < 1e-5);
        let world = runtime.app().world();
        assert_eq!(
            world.get::<EvaluatedMorphWeights>(entity).unwrap().0,
            vec![-1.]
        );
        let n = world.get::<GeometryOverride>(entity).unwrap().0.normals[0];
        assert!((n[1] + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        assert_eq!(
            world.get::<MorphWeights>(entity).unwrap().weights,
            vec![-1.]
        );
    }
}
#[test]
fn morph_playback_seek_loop_pause_removal_and_foreign_writer_are_deterministic() {
    use engine_project::simulation::{self, EvaluatedMorphWeights, MorphPlayer, MorphWeights};
    let (_dir, mut runtime) = morph_runtime(false);
    let entity = runtime.entity("primitive").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .insert(MorphPlayer {
            playing: false,
            time: 0.5,
            ..Default::default()
        });
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!((morph_position(&runtime)[0] - 1.).abs() < 1e-5);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MorphPlayer>(entity)
        .unwrap()
        .time = 1.25;
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!((morph_position(&runtime)[0] + 0.5).abs() < 1e-5);
    {
        let world = runtime.app_mut().world_mut();
        let mut player = world.get_mut::<MorphPlayer>(entity).unwrap();
        player.playing = true;
        player.looping = false;
        player.time = 0.;
    }
    simulation::step(runtime.app_mut().world_mut(), 0.5).unwrap();
    assert!((morph_position(&runtime)[0] - 1.).abs() < 1e-5);
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .remove::<MorphPlayer>();
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!((morph_position(&runtime)[0] - 1.5).abs() < 1e-5);
    let own = runtime
        .app()
        .world()
        .get::<GeometryOverride>(entity)
        .unwrap()
        .0
        .clone();
    let foreign = std::sync::Arc::new((*own).clone());
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .insert(GeometryOverride(foreign.clone()));
    assert!(simulation::prepare(runtime.app_mut().world_mut()).is_err());
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .remove::<MorphWeights>();
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &runtime
            .app()
            .world()
            .get::<GeometryOverride>(entity)
            .unwrap()
            .0,
        &foreign
    ));
    assert!(
        runtime
            .app()
            .world()
            .get::<EvaluatedMorphWeights>(entity)
            .is_none()
    );
}

#[test]
fn morph_delta_is_scaled_by_skin_instead_of_added_after_skin() {
    use engine_project::{Transform, simulation};
    let (_dir, mut runtime) = morph_runtime(true);
    let joint = runtime.entity("node2").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(joint)
        .unwrap()
        .scale = [2., 1., 1.];
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    // Palette translation=-4, scale=2; source position=0 plus default delta=1.5.
    assert!((morph_position(&runtime)[0] + 1.).abs() < 1e-5);
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(joint)
        .get_mut::<Transform>()
        .unwrap()
        .scale = [3., 1., 1.];
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!((morph_position(&runtime)[0] - 0.5).abs() < 1e-5);
}

#[test]
fn morph_model_rebinding_evicts_unused_cache_entry_before_capacity_check() {
    use engine_project::simulation::{self, MorphWeights};
    let (dir, runtime) = morph_runtime(false);
    drop(runtime);
    let path = dir.path().join("project.json");
    let mut project: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for index in 0..65 {
        project["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":format!("rig-{index}"),"kind":"model","path":"model.json"}));
    }
    std::fs::write(path, serde_json::to_vec(&project).unwrap()).unwrap();
    let path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let primitive = scene["entities"].as_array_mut().unwrap().pop().unwrap();
    for index in 0..64 {
        let mut entity = primitive.clone();
        entity["id"] = json!(format!("primitive-{index}"));
        entity["components"]["custom"]["engine.MorphWeights"]["data"]["model"] =
            json!(format!("rig-{index}"));
        scene["entities"].as_array_mut().unwrap().push(entity);
    }
    std::fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    let entity = runtime.entity("primitive-63").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MorphWeights>(entity)
        .unwrap()
        .model = "rig-64".into();
    simulation::prepare(runtime.app_mut().world_mut()).unwrap();
    assert!(
        (runtime
            .app()
            .world()
            .get::<GeometryOverride>(entity)
            .unwrap()
            .0
            .vertices[0]
            .position[0]
            - 1.5)
            .abs()
            < 1e-5
    );
}

#[test]
fn morph_components_require_enabled_simulation_transform_and_weight_binding() {
    use engine_project::{
        Transform,
        simulation::{self, MorphPlayer, MorphWeights},
    };
    let mut world = World::new();
    let owner = world
        .spawn((Transform::default(), MorphPlayer::default()))
        .id();
    assert!(simulation::prepare(&mut world).is_err());
    simulation::install(&mut world, Some(Default::default())).unwrap();
    assert!(simulation::prepare(&mut world).is_err());
    world
        .entity_mut(owner)
        .insert(MorphWeights::default())
        .remove::<Transform>();
    assert!(simulation::prepare(&mut world).is_err());
    let catalog = engine_project::ComponentCatalog::new(vec![]).unwrap();
    assert!(
        catalog.0["engine.MorphPlayer"]
            .requires
            .iter()
            .any(|r| r == "engine.MorphWeights")
    );
}
