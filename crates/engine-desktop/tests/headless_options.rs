//! Option validation and reporting for the headless runner. Headless mode never
//! creates a window, GPU device, gamepad service or audio device.

use engine_core::{Stage, bevy_ecs::prelude::*};
use engine_desktop::{FIXED_DELTA, RunOptions, run};
use engine_project::{ActionFrame, Project, Runtime};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn runtime() -> Runtime {
    Runtime::new(
        Project::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/assets/project.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn headless(ticks: u64) -> RunOptions {
    RunOptions {
        headless: true,
        ticks,
        ..Default::default()
    }
}

#[derive(Resource, Clone, Default)]
struct TickLog(Arc<Mutex<Vec<bool>>>);
fn log_right(input: Res<ActionFrame>, log: Res<TickLog>) {
    log.0.lock().unwrap().push(input.held("right"));
}

fn error_text(result: anyhow::Result<engine_desktop::RunReport>) -> String {
    format!("{:#}", result.err().expect("run must fail"))
}

#[test]
fn zero_ticks_is_rejected_with_the_allowed_range() {
    assert!(error_text(run(runtime(), headless(0))).contains("1..=1000000"));
}

#[test]
fn more_than_one_million_ticks_is_rejected() {
    assert!(error_text(run(runtime(), headless(1_000_001))).contains("1..=1000000"));
}

#[test]
fn input_scheduled_on_the_final_tick_boundary_is_rejected() {
    let options = RunOptions {
        inputs: BTreeMap::from([(3, BTreeSet::new())]),
        ..headless(3)
    };
    assert!(error_text(run(runtime(), options)).contains("input tick 3 exceeds run length"));
}

#[test]
fn input_on_the_last_valid_tick_is_accepted() {
    let options = RunOptions {
        inputs: BTreeMap::from([(2, BTreeSet::from(["right".to_string()]))]),
        ..headless(3)
    };
    assert_eq!(run(runtime(), options).unwrap().ticks, 3);
}

#[test]
fn unknown_injected_action_names_the_action_in_the_error() {
    let options = RunOptions {
        inputs: BTreeMap::from([(0, BTreeSet::from(["levitate".to_string()]))]),
        ..headless(1)
    };
    assert!(error_text(run(runtime(), options)).contains("unknown injected action levitate"));
}

#[test]
fn one_bad_action_among_valid_ones_still_rejects_the_whole_run() {
    let options = RunOptions {
        inputs: BTreeMap::from([(0, BTreeSet::from(["right".to_string(), "nope".to_string()]))]),
        ..headless(2)
    };
    assert!(run(runtime(), options).is_err());
}

#[test]
fn report_contains_the_requested_tick_count_and_fixed_step() {
    let report = run(runtime(), headless(7)).unwrap();
    assert_eq!(report.ticks, 7);
    assert_eq!(report.fixed_delta, FIXED_DELTA);
    assert_eq!(report.presented_frames, 0);
    assert_eq!(report.scene.ticks, 7);
}

#[test]
fn single_tick_run_completes() {
    let report = run(runtime(), headless(1)).unwrap();
    assert_eq!(report.ticks, 1);
}

#[test]
fn a_run_with_no_injected_input_holds_nothing() {
    let mut runtime = runtime();
    let log = TickLog::default();
    runtime.app_mut().world_mut().insert_resource(log.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(log_right);
    run(runtime, headless(4)).unwrap();
    assert_eq!(*log.0.lock().unwrap(), [false; 4]);
}

#[test]
fn injected_hold_is_visible_exactly_on_the_listed_ticks() {
    let mut runtime = runtime();
    let log = TickLog::default();
    runtime.app_mut().world_mut().insert_resource(log.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(log_right);
    let right = BTreeSet::from(["right".to_string()]);
    let options = RunOptions {
        inputs: BTreeMap::from([(1, right.clone()), (3, right)]),
        ..headless(5)
    };
    run(runtime, options).unwrap();
    assert_eq!(*log.0.lock().unwrap(), [false, true, false, true, false]);
}

#[test]
fn shutdown_flag_set_before_start_runs_zero_ticks() {
    let options = RunOptions {
        shutdown: Some(Arc::new(AtomicBool::new(true))),
        ..headless(50)
    };
    let report = run(runtime(), options).unwrap();
    assert_eq!(report.ticks, 0);
}

#[test]
fn unset_shutdown_flag_does_not_interrupt_the_run() {
    let flag = Arc::new(AtomicBool::new(false));
    let options = RunOptions {
        shutdown: Some(flag.clone()),
        ..headless(6)
    };
    assert_eq!(run(runtime(), options).unwrap().ticks, 6);
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn shutdown_requested_by_a_system_stops_between_ticks() {
    #[derive(Resource, Clone)]
    struct Flag(Arc<AtomicBool>);
    fn trip(flag: Res<Flag>, mut count: Local<u32>) {
        *count += 1;
        if *count == 3 {
            flag.0.store(true, Ordering::Release);
        }
    }
    let flag = Arc::new(AtomicBool::new(false));
    let mut runtime = runtime();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Flag(flag.clone()));
    runtime
        .app_mut()
        .schedule_mut(Stage::Update)
        .add_systems(trip);
    let options = RunOptions {
        shutdown: Some(flag),
        ..headless(100)
    };
    assert_eq!(run(runtime, options).unwrap().ticks, 3);
}

#[test]
fn shutdown_stage_runs_once_even_when_the_run_is_cut_short() {
    #[derive(Resource, Clone, Default)]
    struct Count(Arc<Mutex<u32>>);
    fn bump(count: Res<Count>) {
        *count.0.lock().unwrap() += 1;
    }
    let mut runtime = runtime();
    let count = Count::default();
    runtime.app_mut().world_mut().insert_resource(count.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Shutdown)
        .add_systems(bump);
    let options = RunOptions {
        shutdown: Some(Arc::new(AtomicBool::new(true))),
        ..headless(10)
    };
    run(runtime, options).unwrap();
    assert_eq!(*count.0.lock().unwrap(), 1);
}

#[test]
fn rejected_options_never_run_the_shutdown_stage() {
    #[derive(Resource, Clone, Default)]
    struct Count(Arc<Mutex<u32>>);
    fn bump(count: Res<Count>) {
        *count.0.lock().unwrap() += 1;
    }
    let mut runtime = runtime();
    let count = Count::default();
    runtime.app_mut().world_mut().insert_resource(count.clone());
    runtime
        .app_mut()
        .schedule_mut(Stage::Shutdown)
        .add_systems(bump);
    assert!(run(runtime, headless(0)).is_err());
    assert_eq!(
        *count.0.lock().unwrap(),
        0,
        "validation happens before the runtime starts"
    );
}
