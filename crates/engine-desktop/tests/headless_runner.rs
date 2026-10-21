use engine_core::{Stage, bevy_ecs::prelude::*};
use engine_desktop::{RunOptions, run};
use engine_project::{ActionFrame, Project, Runtime};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

#[derive(Resource, Clone, Default)]
struct Observations(Arc<Mutex<Vec<(bool, bool, bool)>>>);
fn record(input: Res<ActionFrame>, observed: Res<Observations>) {
    observed.0.lock().unwrap().push((
        input.pressed("right"),
        input.held("right"),
        input.released("right"),
    ));
}
#[derive(Resource, Clone, Default)]
struct Shutdowns(Arc<Mutex<u32>>);
fn shutdown(observed: Res<Shutdowns>) {
    *observed.0.lock().unwrap() += 1;
}
fn runtime() -> Runtime {
    Runtime::new(
        Project::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/project.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn injected_actions_hold_release_and_shutdown_once_without_devices() {
    let mut runtime = runtime();
    let observed = Observations::default();
    let stopped = Shutdowns::default();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(observed.clone());
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(stopped.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(record);
    runtime
        .app_mut()
        .schedule_mut(Stage::Shutdown)
        .add_systems(shutdown);
    let right = BTreeSet::from(["right".into()]);
    let report = run(
        runtime,
        RunOptions {
            headless: true,
            ticks: 4,
            inputs: BTreeMap::from([(0, right.clone()), (1, right)]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.ticks, 4);
    assert_eq!(report.presented_frames, 0);
    assert_eq!(
        *observed.0.lock().unwrap(),
        vec![
            (true, true, false),
            (false, true, false),
            (false, false, true),
            (false, false, false)
        ]
    );
    assert_eq!(*stopped.0.lock().unwrap(), 1);
}

#[test]
fn generation_stop_runs_cleanup_before_first_tick() {
    let mut runtime = runtime();
    let stopped = Shutdowns::default();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(stopped.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Shutdown)
        .add_systems(shutdown);
    let report = run(
        runtime,
        RunOptions {
            headless: true,
            ticks: 100,
            shutdown: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.ticks, 0);
    assert_eq!(*stopped.0.lock().unwrap(), 1);
}

#[test]
fn invalid_input_and_tick_limits_are_errors() {
    assert!(
        run(
            runtime(),
            RunOptions {
                headless: true,
                ticks: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        run(
            runtime(),
            RunOptions {
                headless: true,
                ticks: 1,
                inputs: BTreeMap::from([(1, BTreeSet::new())]),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        run(
            runtime(),
            RunOptions {
                headless: true,
                ticks: 1,
                inputs: BTreeMap::from([(0, BTreeSet::from(["unknown".into()]))]),
                ..Default::default()
            }
        )
        .is_err()
    );
}
