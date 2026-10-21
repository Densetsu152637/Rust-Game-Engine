use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use engine_core::prelude::*;

#[derive(Component, Debug)]
struct Distance(f32);

#[derive(Component)]
struct Velocity(f32);

#[derive(Bundle)]
struct MovingEntity {
    distance: Distance,
    velocity: Velocity,
}

#[derive(Resource, Default)]
struct Totals {
    startups: usize,
    updates: usize,
    shutdowns: usize,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum GameSet {
    Move,
    Count,
}

fn advance(time: Res<Time>, mut query: Query<(&mut Distance, &Velocity)>) {
    for (mut distance, velocity) in &mut query {
        distance.0 += velocity.0 * time.dt;
    }
}

#[test]
fn native_components_bundle_resources_and_labels_run_deterministically() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Totals>();
    let entity = app
        .world_mut()
        .spawn(MovingEntity {
            distance: Distance(0.0),
            velocity: Velocity(2.0),
        })
        .id();
    app.schedule_mut(Stage::Startup)
        .add_systems(|mut totals: ResMut<Totals>| totals.startups += 1);
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut totals: ResMut<Totals>| totals.shutdowns += 1);
    app.schedule_mut(Stage::Update)
        .configure_sets((GameSet::Move, GameSet::Count).chain())
        .add_systems((
            advance.in_set(GameSet::Move),
            (|query: Query<&Distance>, mut totals: ResMut<Totals>| {
                assert!(query.iter().all(|distance| distance.0 > 0.0));
                totals.updates += 1;
            })
            .in_set(GameSet::Count),
        ));
    app.run_fixed(4, 0.25)?;
    assert_eq!(app.world().get::<Distance>(entity).unwrap().0, 2.0);
    assert_eq!((app.time().tick, app.time().elapsed), (4, 1.0));
    let totals = app.world().resource::<Totals>();
    assert_eq!(
        (totals.startups, totals.updates, totals.shutdowns),
        (1, 4, 1)
    );
    assert_eq!(app.state(), AppState::Stopped);
    app.shutdown()?;
    assert_eq!(app.world().resource::<Totals>().shutdowns, 1);
    assert!(app.tick(0.25).is_err());
    Ok(())
}

#[test]
fn ordered_commands_are_visible_to_dependent_native_systems() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Totals>();
    app.schedule_mut(Stage::Update).add_systems(
        (
            |mut commands: Commands| {
                commands.spawn(Distance(7.0));
            },
            |query: Query<&Distance>, mut totals: ResMut<Totals>| {
                totals.updates = query.iter().count();
            },
        )
            .chain(),
    );
    app.tick(0.1)?;
    assert_eq!(app.world().resource::<Totals>().updates, 1);
    app.shutdown()
}

#[test]
fn cycles_and_ambiguous_conflicts_return_stage_diagnostics() {
    let mut cyclic = EngineApp::new();
    cyclic
        .schedule_mut(Stage::Update)
        .configure_sets((
            GameSet::Move.before(GameSet::Count),
            GameSet::Count.before(GameSet::Move),
        ))
        .add_systems((
            advance.in_set(GameSet::Move),
            (|| {}).in_set(GameSet::Count),
        ));
    let error = cyclic.tick(0.1).unwrap_err().to_string();
    assert!(error.contains("Update schedule is invalid"), "{error}");
    assert_eq!(cyclic.state(), AppState::Failed);

    let mut ambiguous = EngineApp::new();
    ambiguous.world_mut().init_resource::<Totals>();
    ambiguous.schedule_mut(Stage::Update).add_systems((
        |mut totals: ResMut<Totals>| totals.updates += 1,
        |mut totals: ResMut<Totals>| totals.updates += 2,
    ));
    assert!(ambiguous.tick(0.1).is_err());
    assert_eq!(ambiguous.world().resource::<Totals>().updates, 0);
    assert_eq!(ambiguous.state(), AppState::Failed);
}

#[test]
fn zero_ticks_still_runs_lifecycle_and_invalid_delta_does_not_start() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Totals>();
    app.schedule_mut(Stage::Startup)
        .add_systems(|mut t: ResMut<Totals>| t.startups += 1);
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut t: ResMut<Totals>| t.shutdowns += 1);
    for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(app.tick(dt).is_err());
    }
    assert_eq!(app.state(), AppState::Configuring);
    app.run_fixed(0, 0.1)?;
    let totals = app.world().resource::<Totals>();
    assert_eq!(
        (totals.startups, totals.updates, totals.shutdowns),
        (1, 0, 1)
    );
    assert_eq!(app.time().tick, 0);
    Ok(())
}

#[test]
fn failed_startup_still_runs_shutdown_once_in_bounded_runner() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Totals>();
    app.schedule_mut(Stage::Startup).add_systems((
        |mut totals: ResMut<Totals>| totals.startups += 1,
        |mut totals: ResMut<Totals>| totals.startups += 2,
    ));
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut totals: ResMut<Totals>| totals.shutdowns += 1);
    let error = app.run_fixed(2, 0.1).unwrap_err().to_string();
    assert!(error.contains("Startup"));
    assert_eq!(app.world().resource::<Totals>().startups, 0);
    assert_eq!(app.world().resource::<Totals>().shutdowns, 1);
    assert_eq!(app.state(), AppState::Stopped);
    app.shutdown()?;
    assert_eq!(app.world().resource::<Totals>().shutdowns, 1);
    Ok(())
}

#[test]
fn stop_after_update_skips_extraction_and_render_and_shuts_down() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().init_resource::<Totals>();
    app.schedule_mut(Stage::Update)
        .add_systems(|mut control: ResMut<RuntimeControl>| control.request_stop());
    app.schedule_mut(Stage::Extract)
        .add_systems(|| panic!("must not extract after stop"));
    app.schedule_mut(Stage::Render)
        .add_systems(|| panic!("must not render after stop"));
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut totals: ResMut<Totals>| totals.shutdowns += 1);
    app.run_fixed(100, 0.1)?;
    assert_eq!(app.time().tick, 1);
    assert_eq!(app.world().resource::<Totals>().shutdowns, 1);
    Ok(())
}

#[test]
fn render_system_can_request_stop_and_observe_time() -> Result<()> {
    let mut app = EngineApp::new();
    app.schedule_mut(Stage::Render).add_systems(
        |time: Res<Time>, mut control: ResMut<RuntimeControl>| {
            assert_eq!(time.tick, 1);
            control.request_stop();
        },
    );
    app.run_fixed(100, 0.1)?;
    assert_eq!(app.time().tick, 1);
    assert_eq!(app.state(), AppState::Stopped);
    Ok(())
}

#[derive(Component)]
struct TrackedValue {
    value: usize,
    copies: Arc<AtomicUsize>,
}
impl Clone for TrackedValue {
    fn clone(&self) -> Self {
        self.copies.fetch_add(1, Ordering::SeqCst);
        Self {
            value: self.value,
            copies: Arc::clone(&self.copies),
        }
    }
}

#[test]
fn extraction_clones_changes_only_and_propagates_removal_and_generations() -> Result<()> {
    let copies = Arc::new(AtomicUsize::new(0));
    let mut app = EngineApp::new();
    app.extract_component::<TrackedValue>();
    app.extract_component::<TrackedValue>(); // Registration is idempotent.
    let source = app
        .world_mut()
        .spawn(TrackedValue {
            value: 1,
            copies: Arc::clone(&copies),
        })
        .id();
    app.tick(0.1)?;
    let rendered = app.render_snapshot().entity_for(source).unwrap();
    assert_eq!(
        app.render_world()
            .get::<TrackedValue>(rendered)
            .unwrap()
            .value,
        1
    );
    app.tick(0.1)?;
    assert_eq!(copies.load(Ordering::SeqCst), 1);
    app.world_mut()
        .get_mut::<TrackedValue>(source)
        .unwrap()
        .value = 2;
    // Snapshot stays unchanged until the next extraction boundary.
    assert_eq!(
        app.render_world()
            .get::<TrackedValue>(rendered)
            .unwrap()
            .value,
        1
    );
    app.tick(0.1)?;
    assert_eq!(copies.load(Ordering::SeqCst), 2);
    assert_eq!(
        app.render_world()
            .get::<TrackedValue>(rendered)
            .unwrap()
            .value,
        2
    );
    app.world_mut().entity_mut(source).remove::<TrackedValue>();
    app.tick(0.1)?;
    assert!(app.render_world().get::<TrackedValue>(rendered).is_none());
    app.world_mut().despawn(source);
    let replacement = app
        .world_mut()
        .spawn(TrackedValue {
            value: 9,
            copies: Arc::clone(&copies),
        })
        .id();
    assert_ne!(replacement, source);
    app.tick(0.1)?;
    assert!(app.render_snapshot().entity_for(source).is_none());
    assert!(app.render_world().get_entity(rendered).is_err());
    let new_rendered = app.render_snapshot().entity_for(replacement).unwrap();
    assert_eq!(
        app.render_world()
            .get::<TrackedValue>(new_rendered)
            .unwrap()
            .value,
        9
    );
    app.shutdown()
}

#[test]
fn extraction_graph_failure_restores_snapshot_and_marks_failed() {
    let mut app = EngineApp::new();
    app.schedule_mut(Stage::Extract)
        .add_systems((|_: ResMut<RenderWorld>| {}, |_: ResMut<RenderWorld>| {}));
    assert!(app.tick(0.1).is_err());
    assert_eq!(app.state(), AppState::Failed);
    assert!(!app.world().contains_resource::<RenderWorld>());
}

#[test]
fn removing_reserved_render_resource_marks_failed() {
    let mut app = EngineApp::new();
    app.schedule_mut(Stage::Extract).add_systems(
        (|mut commands: Commands| commands.remove_resource::<RenderWorld>())
            .after(ExtractionSet::Components),
    );
    let error = app.tick(0.1).unwrap_err().to_string();
    assert!(
        error.contains("removed the reserved RenderWorld"),
        "{error}"
    );
    assert_eq!(app.state(), AppState::Failed);
}

#[derive(Resource, Default)]
struct Observed(Vec<i32>);

#[test]
fn buffered_publication_keeps_old_reads_until_explicit_graph_edge() -> Result<()> {
    let mut app = EngineApp::new();
    app.world_mut().spawn((Current(1_i32), Next(1_i32)));
    app.world_mut().init_resource::<Observed>();
    app.schedule_mut(Stage::Update).add_systems(
        (
            |mut query: Query<&mut Next<i32>>| {
                for mut next in &mut query {
                    next.0 = 2;
                }
            },
            |query: Query<&Current<i32>>, mut seen: ResMut<Observed>| {
                seen.0.extend(query.iter().map(|c| c.0));
            },
            publish_buffered::<i32>,
            |query: Query<&Current<i32>>, mut seen: ResMut<Observed>| {
                seen.0.extend(query.iter().map(|c| c.0));
            },
        )
            .chain(),
    );
    app.tick(0.1)?;
    assert_eq!(app.world().resource::<Observed>().0, [1, 2]);
    app.shutdown()
}
