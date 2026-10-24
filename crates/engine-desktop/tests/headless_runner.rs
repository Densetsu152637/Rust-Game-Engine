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

#[test]
fn scheduled_callbacks_execute_without_devices_and_remain_in_trace_after_later_ticks() {
    use engine_project::{
        EventBinding, EventBindings, EventSchedule, Transform, register_event_handler,
    };
    let mut runtime = runtime();
    let id = runtime
        .project()
        .scene
        .entities
        .iter()
        .find(|e| e.components.transform.is_some())
        .unwrap()
        .id
        .clone();
    let entity = runtime.entity(&id).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(entity)
        .insert(EventBindings {
            bindings: vec![EventBinding {
                event: "test.set".into(),
                handler: "set".into(),
            }],
        });
    register_event_handler(runtime.app_mut(), "set", |world, entity, event| {
        world.get_mut::<Transform>(entity).unwrap().position[0] =
            event.payload.as_f64().unwrap() as f32;
        engine_project::emit_event(
            world,
            engine_project::EngineEvent {
                name: "test.applied".into(),
                source: Some(entity),
                target: Some(entity),
                payload: 12.into(),
            },
        )?;
        Ok(())
    })
    .unwrap();
    let events = EventSchedule::new(vec![engine_project::ScheduledEvent {
        tick: 0,
        name: "test.set".into(),
        source: None,
        target: Some(id.clone()),
        payload: 12.into(),
    }])
    .unwrap();
    let report = run(
        runtime,
        RunOptions {
            headless: true,
            ticks: 3,
            events,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        report
            .scene
            .scene
            .entities
            .iter()
            .find(|e| e.id == id)
            .unwrap()
            .components
            .transform
            .as_ref()
            .unwrap()
            .position[0],
        12.0
    );
    assert_eq!(report.event_trace.events[0].tick, 0);
    assert_eq!(
        report.event_trace.events[0].target.as_deref(),
        Some(id.as_str())
    );
    assert_eq!(report.event_trace.events[0].payload.as_i64(), Some(12));
    assert!(!report.event_trace.truncated);
    assert!(
        report
            .event_trace
            .events
            .iter()
            .any(|event| event.tick == 1 && event.name == "test.applied"),
        "trace must include deferred callback output, not merely scheduled input"
    );
    assert!(
        report
            .scene
            .events
            .iter()
            .all(|event| event.name != "test.set")
    );
}
