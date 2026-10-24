use engine_core::{MAX_PROFILE_CAPACITY, prelude::*};
use std::time::Duration;

#[derive(Component, Clone)]
struct Visible;

#[test]
fn measures_real_cpu_work_and_counts_both_worlds() {
    let mut app = EngineApp::new();
    app.extract_component::<Visible>();
    app.schedule_mut(Stage::Update)
        .add_systems(|| std::thread::sleep(Duration::from_millis(2)));
    app.tick(0.5).unwrap();
    let baseline = app.profile().frames[0].clone();
    app.world_mut().spawn(Visible);
    app.world_mut().spawn_empty();
    app.tick(0.5).unwrap();
    let snapshot = app.profile();
    let frame = &snapshot.frames[1];
    assert_eq!(frame.tick, 2);
    assert_eq!(
        (frame.simulation_entities, frame.render_entities),
        (
            baseline.simulation_entities + 2,
            baseline.render_entities + 1
        )
    );
    assert_eq!(
        frame
            .stages
            .iter()
            .map(|stage| stage.stage)
            .collect::<Vec<_>>(),
        [
            Stage::Update,
            Stage::PostUpdate,
            Stage::Extract,
            Stage::Render
        ]
    );
    assert!(frame.stages[0].duration_ns >= 2_000_000);
    assert!(frame.duration_ns >= frame.stages.iter().map(|stage| stage.duration_ns).sum());
    assert_eq!(app.time().elapsed, 1.0);
}

#[test]
fn retains_newest_frames_and_aggregates_evicted_samples() {
    let mut app = EngineApp::new();
    app.set_profile_capacity(2).unwrap();
    app.run_fixed(5, 0.1).unwrap();
    let snapshot = app.profile();
    assert_eq!(
        snapshot
            .frames
            .iter()
            .map(|frame| frame.tick)
            .collect::<Vec<_>>(),
        [4, 5]
    );
    assert_eq!((snapshot.frames_recorded, snapshot.frames_dropped), (5, 3));
    for stage in snapshot.stages {
        let expected = if matches!(stage.stage, Stage::Startup | Stage::Shutdown) {
            1
        } else {
            5
        };
        assert_eq!(stage.calls, expected);
        assert_eq!(stage.failures, 0);
        assert!(stage.total_ns >= stage.max_ns);
        assert!(stage.max_ns >= stage.last_ns);
    }
}

#[test]
fn resizing_preserves_newest_history_and_rejects_excessive_capacity() {
    let mut app = EngineApp::new();
    for _ in 0..4 {
        app.tick(0.1).unwrap();
    }
    app.set_profile_capacity(1).unwrap();
    assert_eq!(app.profile().frames[0].tick, 4);
    assert_eq!(app.profile().frames_dropped, 3);
    assert!(app.set_profile_capacity(MAX_PROFILE_CAPACITY + 1).is_err());
    assert_eq!(app.profile().capacity, 1);
    app.set_profile_capacity(0).unwrap();
    app.tick(0.1).unwrap();
    assert!(app.profile().frames.is_empty());
    assert_eq!(app.profile().frames_recorded, 5);
    assert_eq!(app.profile().frames_dropped, 5);
    app.set_profile_capacity(2).unwrap();
    app.tick(0.1).unwrap();
    assert_eq!(app.profile().frames[0].tick, 6);
}

#[test]
fn entity_counters_decrease_after_despawn_instead_of_counting_allocated_slots() {
    let mut app = EngineApp::new();
    app.tick(0.1).unwrap();
    let baseline = app.profile().frames[0].simulation_entities;
    let entity = app.world_mut().spawn_empty().id();
    app.tick(0.1).unwrap();
    assert_eq!(app.profile().frames[1].simulation_entities, baseline + 1);
    app.world_mut().despawn(entity);
    app.tick(0.1).unwrap();
    assert_eq!(app.profile().frames[2].simulation_entities, baseline);
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum Cycle {
    A,
    B,
}

fn invalid_graph(app: &mut EngineApp, stage: Stage) {
    app.schedule_mut(stage)
        .configure_sets((Cycle::A.before(Cycle::B), Cycle::B.before(Cycle::A)))
        .add_systems(((|| {}).in_set(Cycle::A), (|| {}).in_set(Cycle::B)));
}

#[test]
fn records_failed_stage_and_partial_frame_without_masking_failure() {
    let mut app = EngineApp::new();
    invalid_graph(&mut app, Stage::Extract);
    assert!(app.tick(0.1).is_err());
    assert_eq!(app.state(), AppState::Failed);
    let snapshot = app.profile();
    assert_eq!(snapshot.failed_frames, 1);
    assert!(!snapshot.frames[0].succeeded);
    assert_eq!(snapshot.frames[0].stages.len(), 3);
    assert!(!snapshot.frames[0].stages[2].succeeded);
    assert_eq!(
        snapshot
            .stages
            .iter()
            .find(|s| s.stage == Stage::Extract)
            .unwrap()
            .failures,
        1
    );
    app.shutdown().unwrap();
    assert_eq!(app.profile().frames_recorded, 1);
}

#[test]
fn startup_failure_and_invalid_delta_do_not_invent_frames() {
    let mut app = EngineApp::new();
    assert!(app.tick(f32::NAN).is_err());
    assert!(app.profile().stages.iter().all(|stage| stage.calls == 0));
    invalid_graph(&mut app, Stage::Startup);
    assert!(app.tick(0.1).is_err());
    assert_eq!(app.profile().frames_recorded, 0);
    assert_eq!(app.profile().stages[0].failures, 1);
}

#[test]
fn stop_during_update_records_only_executed_stages_and_shutdown() {
    let mut app = EngineApp::new();
    app.schedule_mut(Stage::Update)
        .add_systems(|mut control: ResMut<RuntimeControl>| control.request_stop());
    app.tick(0.1).unwrap();
    let snapshot = app.profile();
    assert_eq!(
        snapshot.frames[0]
            .stages
            .iter()
            .map(|stage| stage.stage)
            .collect::<Vec<_>>(),
        [Stage::Update, Stage::Shutdown]
    );
    assert!(snapshot.frames[0].succeeded);
    app.shutdown().unwrap();
    assert_eq!(app.profile().stages.last().unwrap().calls, 1);
}
