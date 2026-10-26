//! Actual Runtime regressions for cinematic captures across floating-origin shifts.
use engine_project::{
    rendering3d::Camera3D,
    sequencer::{self, Command, SeekEvents, SequenceEvents},
    world::coordinates::{self, WorldOrigin},
    *,
};
use serde_json::json;
use std::fs;

fn fixture(automatic: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut entities = vec![
        json!({"id":"camera","name":"Camera","components":{"Transform":{"position":[8,0,4]},"Camera3D":{}}}),
        json!({"id":"anchor","name":"Anchor","components":{"Transform":{"position":[20,0,0]}}}),
        json!({"id":"child","name":"Child","parent":"anchor","components":{"Transform":{"position":[3,0,0]}}}),
        json!({"id":"director","name":"Director","components":{"custom":{"engine.SequencePlayer":{"version":1,"data":{"bindings":["camera","child"],"timeline":{"version":1,"duration":2,"tracks":[
            {"binding":0,"property":"translation","channel":{"keys":[{"time":0,"value":[10,0,4]},{"time":2,"value":[14,0,4]}]}},
            {"binding":1,"property":"translation","channel":{"keys":[{"time":0,"value":[1,0,0]},{"time":2,"value":[5,0,0]}]}}
        ],"events":[{"time":0.5,"id":"first"},{"time":1.5,"id":"second"}]}}}}}}),
    ];
    if automatic {
        entities.push(json!({"id":"world","name":"World","components":{"Transform":{"position":[0,0,0]},"custom":{"engine.LargeWorld":{"version":1,"data":{"threshold":1,"snap":1,"anchor":"camera"}}}}}));
    }
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"sequencer-coordinates","startup_scene":"scene.json","assets":[]})
            .to_string(),
    )
    .unwrap();
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut rt = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    rt.start().unwrap();
    rt
}
fn control(rt: &mut Runtime, command: Command) -> engine_project::Result<()> {
    let owner = rt.entity("director").unwrap();
    sequencer::command(rt.app_mut().world_mut(), owner, command)
}
fn absolute_x(rt: &Runtime, id: &str) -> f64 {
    coordinates::absolute_position(rt.app().world(), rt.entity(id).unwrap()).unwrap()[0]
}
fn local_x(rt: &Runtime, id: &str) -> f32 {
    rt.app()
        .world()
        .get::<Transform>(rt.entity(id).unwrap())
        .unwrap()
        .position[0]
}
fn position(rt: &Runtime) -> f64 {
    sequencer::status(rt.app().world(), rt.entity("director").unwrap())
        .unwrap()
        .position
}
fn markers(rt: &Runtime) -> Vec<String> {
    rt.app()
        .world()
        .resource::<SequenceEvents>()
        .0
        .iter()
        .map(|e| e.crossing.marker.id.clone())
        .collect()
}
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-4, "{actual} != {expected}");
}
#[test]
fn active_and_paused_rebases_preserve_path_clock_events_seek_and_stop() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    near(absolute_x(&rt, "camera"), 11.);
    near(absolute_x(&rt, "child"), 22.);
    assert_eq!(markers(&rt), ["first"]);
    coordinates::rebase_now(rt.app_mut().world_mut(), [1024., 0., 0.]).unwrap();
    assert_eq!(position(&rt), 0.5);
    assert_eq!(markers(&rt), ["first"]);
    near(absolute_x(&rt, "camera"), 11.);
    assert_eq!(local_x(&rt, "child"), 2.);
    control(&mut rt, Command::Pause).unwrap();
    for offset in [16.125, -32.375, 1024.125] {
        coordinates::rebase_now(rt.app_mut().world_mut(), [offset, 0., 0.]).unwrap();
    }
    rt.tick(0.25, InputSnapshot::default()).unwrap();
    assert_eq!(position(&rt), 0.5);
    assert!(markers(&rt).is_empty());
    control(
        &mut rt,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Crossed,
        },
    )
    .unwrap();
    near(absolute_x(&rt, "camera"), 13.);
    near(absolute_x(&rt, "child"), 24.);
    assert_eq!(markers(&rt), ["second"]);
    assert_eq!(local_x(&rt, "child"), 4.);
    // The normal renderer observes the translated real camera, not a timeline-only sample.
    let frame = rt.extract_scene_frame().unwrap();
    near(
        f64::from(frame.camera3d.unwrap().world[3][0]) + coordinates::origin(rt.app().world())[0],
        13.,
    );
    control(&mut rt, Command::Stop).unwrap();
    near(absolute_x(&rt, "camera"), 8.);
    near(absolute_x(&rt, "child"), 23.);
    control(&mut rt, Command::Play).unwrap();
    near(absolute_x(&rt, "camera"), 10.); // Stop/replay retains the authored key frame.
}
#[test]
fn automatic_boundary_rebases_work_while_paused_and_playing() {
    let dir = fixture(true);
    let mut rt = runtime(&dir);
    control(&mut rt, Command::Pause).unwrap();
    rt.tick(0.25, InputSnapshot::default()).unwrap();
    assert_eq!(position(&rt), 0.);
    assert_eq!(coordinates::status(rt.app().world()).unwrap().shifts, 1);
    near(absolute_x(&rt, "camera"), 10.);
    assert_eq!(local_x(&rt, "child"), 1.);
    control(&mut rt, Command::Play).unwrap();
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    assert_eq!(markers(&rt), ["first"]);
    rt.tick(0.5, InputSnapshot::default()).unwrap();
    rt.tick(0.25, InputSnapshot::default()).unwrap();
    assert_eq!(position(&rt), 1.25);
    assert_eq!(coordinates::status(rt.app().world()).unwrap().shifts, 2);
    near(absolute_x(&rt, "camera"), 12.5);
    near(absolute_x(&rt, "child"), 23.5);
    control(&mut rt, Command::Stop).unwrap();
    near(absolute_x(&rt, "camera"), 8.);
}
#[test]
fn repeated_unobserved_far_shifts_do_not_accumulate_capture_baseline_drift() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    for offset in [1e12, -1e12, 0.] {
        coordinates::rebase_now(rt.app_mut().world_mut(), [offset, 0., 0.]).unwrap();
    }
    // Rebases themselves use f32 local transforms; the next sequence operation reprojects
    // its f64 bookkeeping rather than falsely accusing those known shifts of an external write.
    control(&mut rt, Command::Pause).unwrap();
    assert_eq!(position(&rt), 0.);
    near(absolute_x(&rt, "camera"), 10.);
    control(&mut rt, Command::Stop).unwrap();
    near(absolute_x(&rt, "camera"), 8.);
}
#[test]
fn direct_origin_write_and_nonzero_initial_frame_use_applied_coordinates() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    rt.app_mut()
        .world_mut()
        .resource_mut::<WorldOrigin>()
        .offset = [64., 0., 0.];
    control(
        &mut rt,
        Command::Seek {
            position: 1.5,
            events: SeekEvents::Silent,
        },
    )
    .unwrap();
    assert_eq!(local_x(&rt, "camera"), 13.); // Pending raw origin has not shifted roots yet.
    rt.tick(0.1, InputSnapshot::default()).unwrap();
    assert_eq!(position(&rt), 1.5);
    near(absolute_x(&rt, "camera"), 13.);
    control(&mut rt, Command::Stop).unwrap();
    near(absolute_x(&rt, "camera"), 8.);
    let mut rt = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    rt.app_mut()
        .world_mut()
        .resource_mut::<WorldOrigin>()
        .offset = [256., 0., 0.];
    rt.start().unwrap();
    near(absolute_x(&rt, "camera"), 266.);
    coordinates::rebase_now(rt.app_mut().world_mut(), [512., 0., 0.]).unwrap();
    control(
        &mut rt,
        Command::Seek {
            position: 1.,
            events: SeekEvents::Silent,
        },
    )
    .unwrap();
    near(absolute_x(&rt, "camera"), 268.);
    control(&mut rt, Command::Stop).unwrap();
    near(absolute_x(&rt, "camera"), 264.);
}
#[test]
fn external_writes_before_and_after_a_rebase_still_conflict_and_are_preserved() {
    for before in [true, false] {
        let dir = fixture(false);
        let mut rt = runtime(&dir);
        let camera = rt.entity("camera").unwrap();
        if before {
            rt.app_mut()
                .world_mut()
                .get_mut::<Transform>(camera)
                .unwrap()
                .position[0] = 77.;
        }
        coordinates::rebase_now(rt.app_mut().world_mut(), [32., 0., 0.]).unwrap();
        if !before {
            rt.app_mut()
                .world_mut()
                .get_mut::<Transform>(camera)
                .unwrap()
                .position[0] = 45.;
        }
        assert!(control(&mut rt, Command::Pause).is_err());
        near(absolute_x(&rt, "camera"), 77.);
    }
}
#[test]
fn reparenting_root_or_child_rejects_without_restoring_into_a_different_frame() {
    let dir = fixture(false);
    let mut rt = runtime(&dir);
    let camera = rt.entity("camera").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(camera)
        .insert(Parent("anchor".into()));
    coordinates::rebase_now(rt.app_mut().world_mut(), [32., 0., 0.]).unwrap();
    assert!(control(&mut rt, Command::Pause).is_err());
    assert_eq!(local_x(&rt, "camera"), 10.);
    let mut rt = runtime(&dir);
    let child = rt.entity("child").unwrap();
    rt.app_mut()
        .world_mut()
        .entity_mut(child)
        .remove::<Parent>();
    coordinates::rebase_now(rt.app_mut().world_mut(), [32., 0., 0.]).unwrap();
    assert!(control(&mut rt, Command::Stop).is_err());
    assert_eq!(local_x(&rt, "child"), -31.);
}
#[test]
fn nonfinite_native_capture_is_rejected_before_any_infinite_projection() {
    let dir = fixture(false);
    let mut rt = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    let camera = rt.entity("camera").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<Transform>(camera)
        .unwrap()
        .position[0] = f32::INFINITY;
    assert!(rt.start().is_err());
    assert!(!rt.running());
    assert!(rt.app().world().get::<Camera3D>(camera).is_some());
}
