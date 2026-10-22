//! App lifecycle: startup, shutdown, cancellation, failure and phase boundaries.
mod common;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use common::*;
use engine_core::prelude::*;

#[derive(Resource, Default, Debug, PartialEq)]
struct Phases {
    startups: u32,
    updates: u32,
    extracts: u32,
    shutdowns: u32,
}

fn lifecycle_app() -> EngineApp {
    let mut app = app();
    app.world_mut().init_resource::<Phases>();
    app.schedule_mut(Stage::Startup)
        .add_systems(|mut p: ResMut<Phases>| p.startups += 1);
    update(&mut app).add_systems(|mut p: ResMut<Phases>| p.updates += 1);
    app.schedule_mut(Stage::Extract)
        .add_systems(|mut p: ResMut<Phases>| p.extracts += 1);
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut p: ResMut<Phases>| p.shutdowns += 1);
    app
}

fn phases(app: &EngineApp) -> (u32, u32, u32, u32) {
    let p = app.world().resource::<Phases>();
    (p.startups, p.updates, p.extracts, p.shutdowns)
}

// ---- state machine ----------------------------------------------------------

#[test]
fn a_new_app_is_configuring_and_not_running() {
    let app = lifecycle_app();
    assert_eq!(app.state(), AppState::Configuring);
    assert!(!app.is_running());
    assert!(!app.stop_requested());
    assert_eq!(phases(&app), (0, 0, 0, 0));
}

#[test]
fn default_and_new_apps_start_in_the_same_state() {
    let a = EngineApp::default();
    let b = EngineApp::new();
    assert_eq!(a.state(), b.state());
    assert_eq!(a.time().tick, b.time().tick);
    assert!(a.world().contains_resource::<Time>());
    assert!(a.world().contains_resource::<RuntimeControl>());
}

#[test]
fn startup_runs_the_startup_stage_once_and_enters_running() {
    let mut app = lifecycle_app();
    app.startup().unwrap();
    assert_eq!(app.state(), AppState::Running);
    assert!(app.is_running());
    assert_eq!(phases(&app), (1, 0, 0, 0));
}

#[test]
fn repeated_startup_calls_are_idempotent() {
    let mut app = lifecycle_app();
    for _ in 0..5 {
        app.startup().unwrap();
    }
    assert_eq!(phases(&app).0, 1);
}

#[test]
fn tick_starts_a_configuring_app_automatically() {
    let mut app = lifecycle_app();
    app.tick(DT).unwrap();
    assert_eq!(phases(&app), (1, 1, 1, 0));
    assert!(app.is_running());
}

#[test]
fn startup_runs_exactly_once_over_many_ticks() {
    let mut app = lifecycle_app();
    for _ in 0..20 {
        app.tick(DT).unwrap();
    }
    assert_eq!(phases(&app), (1, 20, 20, 0));
}

#[test]
fn shutdown_runs_the_shutdown_stage_once_and_enters_stopped() {
    let mut app = lifecycle_app();
    app.startup().unwrap();
    app.shutdown().unwrap();
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(phases(&app), (1, 0, 0, 1));
}

#[test]
fn repeated_shutdown_calls_are_idempotent() {
    let mut app = lifecycle_app();
    app.startup().unwrap();
    for _ in 0..4 {
        app.shutdown().unwrap();
    }
    assert_eq!(phases(&app).3, 1);
}

#[test]
fn shutdown_of_a_never_started_app_skips_shutdown_systems() {
    let mut app = lifecycle_app();
    app.shutdown().unwrap();
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(phases(&app), (0, 0, 0, 0));
}

#[test]
fn a_stopped_app_cannot_be_started_again() {
    let mut app = lifecycle_app();
    app.run_fixed(1, DT).unwrap();
    let error = app.startup().unwrap_err().to_string();
    assert!(error.contains("Stopped"), "{error}");
    assert_eq!(phases(&app).0, 1);
}

#[test]
fn a_stopped_app_cannot_tick() {
    let mut app = lifecycle_app();
    app.run_fixed(2, DT).unwrap();
    assert!(app.tick(DT).is_err());
    assert_eq!(phases(&app), (1, 2, 2, 1));
}

#[test]
fn a_never_started_app_shut_down_cannot_be_ticked_either() {
    let mut app = lifecycle_app();
    app.shutdown().unwrap();
    assert!(app.tick(DT).is_err());
    assert!(app.startup().is_err());
}

#[test]
fn invalid_delta_is_checked_before_state_so_a_stopped_app_reports_the_delta() {
    let mut app = lifecycle_app();
    app.run_fixed(1, DT).unwrap();
    let error = app.tick(f32::NAN).unwrap_err().to_string();
    assert!(error.contains("dt must be finite"), "{error}");
}

#[test]
fn run_fixed_always_ends_stopped_even_for_zero_ticks() {
    let mut app = lifecycle_app();
    app.run_fixed(0, DT).unwrap();
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(phases(&app), (1, 0, 0, 1));
}

#[test]
fn run_fixed_on_a_stopped_app_reports_the_startup_error_and_does_not_rerun_shutdown() {
    let mut app = lifecycle_app();
    app.run_fixed(1, DT).unwrap();
    assert!(app.run_fixed(1, DT).is_err());
    assert_eq!(phases(&app).3, 1);
}

#[test]
fn separate_app_generations_do_not_share_state() {
    let mut first = lifecycle_app();
    let mut second = lifecycle_app();
    first.run_fixed(3, DT).unwrap();
    second.run_fixed(1, DT).unwrap();
    assert_eq!(phases(&first), (1, 3, 3, 1));
    assert_eq!(phases(&second), (1, 1, 1, 1));
    assert_eq!(first.time().tick, 3);
    assert_eq!(second.time().tick, 1);
}

#[test]
fn dropping_an_app_does_not_implicitly_run_shutdown() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let mut app = app();
    app.schedule_mut(Stage::Shutdown)
        .add_systems(move || flag.store(true, Ordering::SeqCst));
    app.startup().unwrap();
    drop(app);
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn explicit_shutdown_does_run_the_shutdown_stage() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let mut app = app();
    app.schedule_mut(Stage::Shutdown)
        .add_systems(move || flag.store(true, Ordering::SeqCst));
    app.startup().unwrap();
    app.shutdown().unwrap();
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn world_remains_inspectable_after_shutdown() {
    let mut app = lifecycle_app();
    let e = app.world_mut().spawn(Health(3)).id();
    app.run_fixed(2, DT).unwrap();
    assert_eq!(app.world().get::<Health>(e), Some(&Health(3)));
    assert_eq!(app.time().tick, 2);
}

// ---- cancellation and phase boundaries -------------------------------------------

#[test]
fn stop_requested_before_the_first_tick_starts_up_then_shuts_down_without_updating() {
    let mut app = lifecycle_app();
    app.request_stop();
    assert!(app.stop_requested());
    app.tick(DT).unwrap();
    assert_eq!(phases(&app), (1, 0, 0, 1));
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(app.time().tick, 0);
}

#[test]
fn stop_requested_between_ticks_stops_before_the_next_update() {
    let mut app = lifecycle_app();
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.request_stop();
    app.tick(DT).unwrap();
    assert_eq!(phases(&app), (1, 2, 2, 1));
    assert_eq!(app.time().tick, 2);
    assert_eq!(app.state(), AppState::Stopped);
}

#[test]
fn host_cancellation_during_run_fixed_is_honoured_by_a_system_flag() {
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&cancel);
    let mut app = lifecycle_app();
    update(&mut app).add_systems(move |t: Res<Time>, mut c: ResMut<RuntimeControl>| {
        if t.tick == 7 {
            seen.store(true, Ordering::SeqCst);
        }
        if seen.load(Ordering::SeqCst) {
            c.request_stop();
        }
    });
    app.run_fixed(1_000, DT).unwrap();
    assert_eq!(app.time().tick, 7);
    assert!(cancel.load(Ordering::SeqCst));
}

#[test]
fn stop_requested_in_startup_prevents_every_update() {
    let mut app = lifecycle_app();
    app.schedule_mut(Stage::Startup)
        .add_systems(|mut c: ResMut<RuntimeControl>| c.request_stop());
    app.run_fixed(10, DT).unwrap();
    assert_eq!(phases(&app), (1, 0, 0, 1));
    assert_eq!(app.time().tick, 0);
}

#[test]
fn stop_in_update_lets_the_whole_update_stage_finish_but_skips_extract() {
    let mut app = lifecycle_app();
    app.world_mut().init_resource::<Log>();
    update(&mut app).add_systems(
        (
            |mut c: ResMut<RuntimeControl>, mut l: ResMut<Log>| {
                c.request_stop();
                l.0.push("first");
            },
            |mut l: ResMut<Log>| l.0.push("second"),
            |mut l: ResMut<Log>| l.0.push("third"),
        )
            .chain(),
    );
    app.run_fixed(5, DT).unwrap();
    assert_eq!(log(&app), ["first", "second", "third"]);
    assert_eq!(
        phases(&app).2,
        0,
        "extract must not run after a stop in update"
    );
    assert_eq!(phases(&app).3, 1);
}

#[test]
fn stop_in_update_applies_pending_commands_before_shutdown() {
    let mut app = lifecycle_app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|mut c: Commands, mut ctl: ResMut<RuntimeControl>| {
        c.spawn(Health(1));
        ctl.request_stop();
    });
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|q: Query<&Health>, mut s: ResMut<Seen>| s.0.push(q.iter().count() as i64));
    app.run_fixed(5, DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1]);
}

#[test]
fn stop_in_extract_skips_render_but_keeps_the_extracted_snapshot() {
    let mut app = lifecycle_app();
    app.extract_component::<Health>();
    app.world_mut().spawn(Health(4));
    app.schedule_mut(Stage::Extract).add_systems(
        (|mut c: ResMut<RuntimeControl>| c.request_stop()).after(ExtractionSet::Components),
    );
    app.schedule_mut(Stage::Render)
        .add_systems(|| panic!("render must not run after a stop in extract"));
    app.run_fixed(5, DT).unwrap();
    assert_eq!(phases(&app).3, 1);
    assert_eq!(count::<Health>(app.render_world_mut()), 1);
}

#[test]
fn stop_in_render_is_propagated_to_the_simulation_world() {
    let mut app = lifecycle_app();
    app.render_world_mut().init_resource::<Counter>();
    app.schedule_mut(Stage::Render).add_systems(
        |t: Res<Time>, mut c: ResMut<Counter>, mut ctl: ResMut<RuntimeControl>| {
            c.0 += 1;
            if t.tick == 3 {
                ctl.request_stop();
            }
        },
    );
    app.run_fixed(100, DT).unwrap();
    assert!(app.stop_requested());
    assert_eq!(app.time().tick, 3);
    assert_eq!(app.render_world().resource::<Counter>().0, 3);
}

#[test]
fn stop_request_is_sticky_and_idempotent() {
    let mut app = lifecycle_app();
    for _ in 0..3 {
        app.request_stop();
    }
    assert!(app.stop_requested());
    assert!(app.world().resource::<RuntimeControl>().stop_requested());
}

#[test]
fn stop_requested_inside_shutdown_does_not_break_idempotence() {
    let mut app = lifecycle_app();
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut c: ResMut<RuntimeControl>| c.request_stop());
    app.run_fixed(2, DT).unwrap();
    app.shutdown().unwrap();
    assert_eq!(phases(&app).3, 1);
    assert_eq!(app.state(), AppState::Stopped);
}

#[test]
fn stop_after_the_final_requested_tick_is_not_an_error() {
    let mut app = lifecycle_app();
    update(&mut app).add_systems(|t: Res<Time>, mut c: ResMut<RuntimeControl>| {
        if t.tick == 4 {
            c.request_stop();
        }
    });
    app.run_fixed(4, DT).unwrap();
    assert_eq!(app.time().tick, 4);
    assert_eq!(app.state(), AppState::Stopped);
}

#[test]
fn render_world_receives_a_runtime_control_snapshot_each_tick() {
    let mut app = lifecycle_app();
    app.render_world_mut().init_resource::<Seen>();
    app.schedule_mut(Stage::Render)
        .add_systems(|c: Res<RuntimeControl>, mut s: ResMut<Seen>| {
            s.0.push(i64::from(c.stop_requested()))
        });
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.render_world().resource::<Seen>().0, [0, 0]);
}

// ---- failure ---------------------------------------------------------------------

fn broken_schedule(app: &mut EngineApp, stage: Stage) {
    app.world_mut().init_resource::<Counter>();
    app.schedule_mut(stage).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut c: ResMut<Counter>| c.0 += 1,
    ));
}

#[test]
fn invalid_startup_graph_fails_the_app_and_blocks_updates() {
    let mut app = lifecycle_app();
    broken_schedule(&mut app, Stage::Startup);
    let error = app.tick(DT).unwrap_err().to_string();
    assert!(error.contains("Startup schedule is invalid"), "{error}");
    assert_eq!(app.state(), AppState::Failed);
    assert_eq!(phases(&app).1, 0);
}

#[test]
fn failed_app_can_still_be_shut_down_exactly_once() {
    let mut app = lifecycle_app();
    broken_schedule(&mut app, Stage::Update);
    assert!(app.tick(DT).is_err());
    assert_eq!(app.state(), AppState::Failed);
    app.shutdown().unwrap();
    app.shutdown().unwrap();
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(phases(&app).3, 1);
}

#[test]
fn failed_app_refuses_to_start_or_tick() {
    let mut app = lifecycle_app();
    broken_schedule(&mut app, Stage::Update);
    assert!(app.tick(DT).is_err());
    let again = app.tick(DT).unwrap_err().to_string();
    assert!(again.contains("Failed"), "{again}");
    assert!(app.startup().is_err());
}

#[test]
fn run_fixed_cleans_up_after_a_runtime_graph_error_and_returns_the_original_error() {
    let mut app = lifecycle_app();
    broken_schedule(&mut app, Stage::Update);
    let error = app.run_fixed(5, DT).unwrap_err().to_string();
    assert!(error.contains("Update schedule is invalid"), "{error}");
    assert_eq!(app.state(), AppState::Stopped);
    assert_eq!(phases(&app).3, 1, "shutdown systems still run");
}

#[test]
fn run_fixed_reports_both_errors_when_cleanup_also_fails() {
    let mut app = lifecycle_app();
    app.world_mut().init_resource::<Counter>();
    for stage in [Stage::Update, Stage::Shutdown] {
        app.schedule_mut(stage).add_systems((
            |mut c: ResMut<Counter>| c.0 += 1,
            |mut c: ResMut<Counter>| c.0 += 1,
        ));
    }
    let error = format!("{:#}", app.run_fixed(3, DT).unwrap_err());
    assert!(error.contains("Update schedule is invalid"), "{error}");
    assert!(error.contains("shutdown also failed"), "{error}");
    assert!(error.contains("Shutdown schedule is invalid"), "{error}");
    assert_eq!(app.state(), AppState::Stopped);
}

#[test]
fn extract_graph_error_keeps_the_update_effects_of_that_tick() {
    let mut app = lifecycle_app();
    app.schedule_mut(Stage::Extract)
        .add_systems((|_: ResMut<RenderWorld>| {}, |_: ResMut<RenderWorld>| {}));
    assert!(app.tick(DT).is_err());
    assert_eq!(phases(&app).1, 1);
    assert_eq!(app.state(), AppState::Failed);
}

#[test]
fn render_graph_error_marks_the_app_failed() {
    let mut app = lifecycle_app();
    app.render_world_mut().init_resource::<Counter>();
    app.schedule_mut(Stage::Render).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut c: ResMut<Counter>| c.0 += 1,
    ));
    let error = app.tick(DT).unwrap_err().to_string();
    assert!(error.contains("Render schedule is invalid"), "{error}");
    assert_eq!(app.state(), AppState::Failed);
}

#[test]
fn render_world_is_restored_after_an_extract_failure() {
    let mut app = lifecycle_app();
    app.extract_component::<Health>();
    app.world_mut().spawn(Health(1));
    app.schedule_mut(Stage::Extract)
        .add_systems((|_: ResMut<RenderWorld>| {}, |_: ResMut<RenderWorld>| {}));
    assert!(app.tick(DT).is_err());
    assert!(!app.world().contains_resource::<RenderWorld>());
    // The accessor still works on the restored snapshot.
    assert_eq!(count::<Health>(app.render_world_mut()), 0);
}

#[test]
fn panicking_system_unwinds_to_the_caller_instead_of_being_swallowed() {
    let mut app = lifecycle_app();
    update(&mut app).add_systems(|t: Res<Time>| {
        if t.tick == 2 {
            panic!("boom");
        }
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.run_fixed(5, DT)));
    assert!(outcome.is_err());
    assert_eq!(app.time().tick, 2);
}

#[test]
fn startup_can_seed_the_world_for_the_first_update() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.schedule_mut(Stage::Startup)
        .add_systems(|mut c: Commands| {
            for i in 0..5 {
                c.spawn(Health(i));
            }
        });
    update(&mut app).add_systems(|q: Query<&Health>, mut s: ResMut<Seen>| {
        s.0.push(q.iter().map(|h| i64::from(h.0)).sum());
    });
    app.run_fixed(2, DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [10, 10]);
}

#[test]
fn shutdown_can_observe_final_simulation_state() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|mut c: ResMut<Counter>| c.0 += 3);
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|c: Res<Counter>, mut s: ResMut<Seen>| s.0.push(c.0));
    app.run_fixed(4, DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [12]);
}

#[test]
fn counts_of_system_invocations_match_phase_counts_over_a_long_run() {
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&calls);
    let mut app = lifecycle_app();
    update(&mut app).add_systems(move || {
        probe.fetch_add(1, Ordering::SeqCst);
    });
    app.run_fixed(250, 0.015625).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 250);
    assert_eq!(phases(&app), (1, 250, 250, 1));
}
