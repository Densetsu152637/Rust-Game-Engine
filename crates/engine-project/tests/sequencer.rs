use engine_project::{
    rendering3d::*,
    sequencer::{self, Command, SeekEvents, SequenceEvents, SequencePlayer, State},
    *,
};
use serde_json::{Value, json};
use std::fs;
fn timeline() -> Value {
    json!({"version":1,"duration":2,"tracks":[
    {"binding":0,"property":"translation","channel":{"keys":[{"time":0,"value":[0,0,0]},{"time":2,"value":[4,0,0]}]}},
    {"binding":1,"property":"camera_fov","channel":{"keys":[{"time":0,"value":1},{"time":2,"value":2}]}}
],"camera_cuts":[{"time":0,"binding":1},{"time":1,"binding":2}],"events":[{"time":0.5,"id":"first"},{"time":1,"id":"second"}]})
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let scene = json!({"schema":1,"scene_id":"scene","entities":[
        {"id":"actor","name":"Actor","components":{"Transform":{"position":[8,0,0]}}},
        {"id":"camera_a","name":"A","components":{"Transform":{"position":[0,0,4]},"Camera3D":{}}},
        {"id":"camera_b","name":"B","components":{"Transform":{"position":[0,0,9]},"Camera3D":{"active":false}}},
        {"id":"director","name":"Director","components":{"custom":{"engine.SequencePlayer":{"version":1,"data":{"timeline":timeline(),"bindings":["actor","camera_a","camera_b"]}}}}}
    ]});
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("prefab.json"),
        serde_json::to_vec(&scene).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("project.json"), serde_json::to_vec(&json!({"schema":1,"name":"cinematics-test","startup_scene":"scene.json","assets":[{"id":"shot","kind":"prefab","path":"prefab.json"}]})).unwrap()).unwrap();
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut rt = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    rt.start().unwrap();
    rt
}
fn x(rt: &Runtime) -> f32 {
    rt.app()
        .world()
        .get::<Transform>(rt.entity("actor").unwrap())
        .unwrap()
        .position[0]
}
fn control(rt: &mut Runtime, command: Command) {
    let owner = rt.entity("director").unwrap();
    sequencer::command(rt.app_mut().world_mut(), owner, command).unwrap();
}
#[test]
fn authored_runtime_ticks_seek_and_cuts_change_extracted_camera() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    assert_eq!(x(&rt), 0.);
    assert_eq!(
        rt.extract_scene_frame().unwrap().camera3d.unwrap().world[3][2],
        4.
    );
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    assert_eq!(x(&rt), 1.);
    assert_eq!(
        rt.app().world().resource::<SequenceEvents>().0[0]
            .crossing
            .marker
            .id,
        "first"
    );
    control(&mut rt, Command::Pause);
    rt.tick(0.25, InputSnapshot::default()).unwrap();
    assert_eq!(x(&rt), 1.);
    control(
        &mut rt,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Crossed,
        },
    );
    assert_eq!(x(&rt), 3.);
    assert_eq!(
        rt.app().world().resource::<SequenceEvents>().0[0]
            .crossing
            .marker
            .id,
        "second"
    );
    assert_eq!(
        rt.extract_scene_frame().unwrap().camera3d.unwrap().world[3][2],
        9.
    );
    control(
        &mut rt,
        Command::Seek {
            position: 0.25,
            events: SeekEvents::Silent,
        },
    );
    assert!(rt.app().world().resource::<SequenceEvents>().0.is_empty());
    assert_eq!(x(&rt), 0.5);
    assert_eq!(
        rt.extract_scene_frame().unwrap().camera3d.unwrap().world[3][2],
        4.
    );
    control(&mut rt, Command::Stop);
    assert_eq!(x(&rt), 8.);
    let owner = rt.entity("director").unwrap();
    assert_eq!(
        sequencer::status(rt.app().world(), owner).unwrap().state,
        State::Stopped
    );
}
#[test]
fn removal_and_missing_target_cancel_surviving_fields_and_cameras() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    let owner = rt.entity("director").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(owner)
        .remove::<SequencePlayer>();
    rt.tick(0.1, InputSnapshot::default()).unwrap();
    assert_eq!(x(&rt), 8.);
    let mut rt = runtime(&dir);
    control(
        &mut rt,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Silent,
        },
    );
    let actor = rt.entity("actor").unwrap();
    rt.app_mut().world_mut().despawn(actor);
    assert!(rt.tick(0.1, InputSnapshot::default()).is_err());
    let camera = rt.entity("camera_a").unwrap();
    assert!(rt.app().world().get::<Camera3D>(camera).unwrap().active);
    let camera = rt.entity("camera_b").unwrap();
    assert!(!rt.app().world().get::<Camera3D>(camera).unwrap().active);
}
#[test]
fn remote_and_physics_pose_authority_are_explicit_errors() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    let actor = rt.entity("actor").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(actor)
        .insert(networking::NetworkReplica {
            authority: networking::ReplicaAuthority::Remote,
        });
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
    let mut rt = runtime(&dir);
    let actor = rt.entity("actor").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(actor)
        .insert(simulation::PhysicsBody3D::default());
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
}
#[test]
fn conflicting_directors_and_bound_animation_writers_fail() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    let owner = rt.entity("director").unwrap();
    let config = rt
        .app()
        .world()
        .get::<SequencePlayer>(owner)
        .unwrap()
        .clone();
    rt.app_mut().world_mut().spawn(config);
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
    assert_eq!(x(&rt), 8.);
    let mut rt = runtime(&dir);
    rt.app_mut().world_mut().spawn(simulation::AnimationPlayer {
        bindings: vec!["actor".into()],
        ..Default::default()
    });
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
}
#[test]
fn invalid_authored_timeline_fails_project_load() {
    let dir = fixture();
    let path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    scene["entities"][3]["components"]["custom"]["engine.SequencePlayer"]["data"]["timeline"]["tracks"]
        [0]["binding"] = json!(99);
    fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    assert!(Project::load(dir.path().join("project.json")).is_err());
}
#[test]
fn prefab_binding_indices_remain_local_after_entity_ids_are_remapped() {
    let dir = fixture();
    let mut document = SceneDocument::new(Project::load(dir.path().join("project.json")).unwrap());
    let revision = document.revision().to_owned();
    document
        .apply(
            EditOperation::InstantiatePrefab {
                asset: "shot".into(),
                prefix: "copy:".into(),
                parent: None,
            },
            &revision,
        )
        .unwrap();
    let director = document
        .snapshot()
        .entities
        .iter()
        .find(|e| e.id == "copy:director")
        .unwrap();
    assert_eq!(
        director.components.custom[sequencer::SEQUENCE_PLAYER_COMPONENT].data["bindings"],
        json!(["copy:actor", "copy:camera_a", "copy:camera_b"])
    );
    // Both cut directors must be explicit: globally overlapping camera ownership is rejected.
    let revision = document.revision().to_owned();
    let mut rt = Runtime::new(document.begin_play(&revision).unwrap()).unwrap();
    assert!(rt.start().is_err());
}
#[test]
fn external_field_change_is_preserved_when_sequence_cancels() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    let actor = rt.entity("actor").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(actor)
        .unwrap()
        .position = [77., 0., 0.];
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
    assert_eq!(x(&rt), 77.);
}
#[test]
fn natural_completion_holds_endpoint_until_explicit_stop() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    rt.tick(2., InputSnapshot::default()).unwrap();
    assert_eq!(x(&rt), 4.);
    let owner = rt.entity("director").unwrap();
    assert_eq!(
        sequencer::status(rt.app().world(), owner).unwrap().state,
        State::Finished
    );
    control(&mut rt, Command::Stop);
    assert_eq!(x(&rt), 8.);
}
#[test]
fn ancestor_of_world_space_physics_writer_is_rejected() {
    let dir = fixture();
    let mut rt = runtime(&dir);
    rt.app_mut().world_mut().spawn((
        Transform::default(),
        Parent("actor".into()),
        simulation::PhysicsBody3D::default(),
    ));
    assert!(sequencer::prepare(rt.app_mut().world_mut()).is_err());
}
