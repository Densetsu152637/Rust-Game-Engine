//! System ordering, dependency graphs, conflicting access and invalid graphs.
mod common;

use std::sync::Mutex;

use common::*;
use engine_core::bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings, SystemCondition};
use engine_core::prelude::*;

#[derive(Resource, Default)]
struct Trace(Mutex<Vec<u32>>);

impl Trace {
    fn push(&self, id: u32) {
        self.0.lock().unwrap().push(id);
    }
    fn take(&self) -> Vec<u32> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct Node(u32);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
enum Phase {
    Input,
    Simulate,
    Resolve,
}

#[derive(Component)]
struct A;

fn tracing_app() -> EngineApp {
    let mut app = app();
    app.world_mut().init_resource::<Trace>();
    app
}

fn trace_of(app: &mut EngineApp) -> Vec<u32> {
    app.world().resource::<Trace>().take()
}

fn mark(id: u32) -> impl FnMut(Res<Trace>) {
    move |trace: Res<Trace>| trace.push(id)
}

fn position(trace: &[u32], id: u32) -> usize {
    trace
        .iter()
        .position(|v| *v == id)
        .unwrap_or_else(|| panic!("{id} missing from {trace:?}"))
}

// ---- ordering -------------------------------------------------------------

#[test]
fn chain_runs_systems_in_declaration_order() {
    let mut app = tracing_app();
    update(&mut app).add_systems((mark(1), mark(2), mark(3), mark(4)).chain());
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 2, 3, 4]);
}

#[test]
fn chain_order_is_independent_of_registration_position() {
    let mut app = tracing_app();
    update(&mut app).add_systems(mark(9));
    update(&mut app).add_systems((mark(3), mark(2), mark(1)).chain());
    update(&mut app).add_systems(mark(8));
    app.tick(DT).unwrap();
    let trace = trace_of(&mut app);
    assert!(position(&trace, 3) < position(&trace, 2));
    assert!(position(&trace, 2) < position(&trace, 1));
    assert_eq!(trace.len(), 5);
}

#[test]
fn a_long_chain_of_one_hundred_systems_keeps_exact_order() {
    let mut app = tracing_app();
    for id in 0..100_u32 {
        update(&mut app).add_systems(mark(id).in_set(Node(id)));
        if id > 0 {
            update(&mut app).configure_sets(Node(id - 1).before(Node(id)));
        }
    }
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), (0..100).collect::<Vec<_>>());
}

#[test]
fn before_and_after_between_sets_order_all_members() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets(Node(1).before(Node(2)))
        .add_systems((
            mark(20).in_set(Node(2)),
            mark(21).in_set(Node(2)),
            mark(10).in_set(Node(1)),
            mark(11).in_set(Node(1)),
        ));
    app.tick(DT).unwrap();
    let t = trace_of(&mut app);
    for early in [10, 11] {
        for late in [20, 21] {
            assert!(position(&t, early) < position(&t, late), "{t:?}");
        }
    }
}

#[test]
fn three_phase_sets_chain_transitively() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((Phase::Input, Phase::Simulate, Phase::Resolve).chain())
        .add_systems((
            mark(3).in_set(Phase::Resolve),
            mark(1).in_set(Phase::Input),
            mark(2).in_set(Phase::Simulate),
        ));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 2, 3]);
}

#[test]
fn a_system_may_belong_to_several_sets_and_obey_each() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((Phase::Input, Phase::Resolve).chain())
        .configure_sets(Node(7).after(Phase::Input))
        .add_systems((
            mark(2).in_set(Phase::Resolve).in_set(Node(7)),
            mark(1).in_set(Phase::Input),
        ));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 2]);
}

#[test]
fn nested_sets_inherit_the_ordering_of_their_parent() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((Phase::Input, Phase::Simulate).chain())
        .configure_sets(Node(5).in_set(Phase::Simulate))
        .add_systems((mark(2).in_set(Node(5)), mark(1).in_set(Phase::Input)));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 2]);
}

#[test]
fn function_systems_can_be_ordered_by_their_function_names() {
    fn first(t: Res<Trace>) {
        t.push(1);
    }
    fn second(t: Res<Trace>) {
        t.push(2);
    }
    let mut app = tracing_app();
    update(&mut app).add_systems((second.after(first), first));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 2]);
}

#[test]
fn each_system_runs_exactly_once_per_tick() {
    let mut app = tracing_app();
    update(&mut app).add_systems((mark(1), mark(2), mark(3)));
    app.run_fixed(5, DT).unwrap();
    let mut trace = trace_of(&mut app);
    trace.sort();
    assert_eq!(trace, [1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3]);
}

#[test]
fn ordering_holds_on_every_tick_not_just_the_first() {
    let mut app = tracing_app();
    update(&mut app).add_systems((mark(1), mark(2), mark(3)).chain());
    app.run_fixed(6, DT).unwrap();
    let trace = trace_of(&mut app);
    assert_eq!(trace, [1, 2, 3].repeat(6));
}

#[test]
fn systems_added_between_ticks_join_the_graph_and_obey_edges() {
    let mut app = tracing_app();
    update(&mut app).add_systems(mark(1).in_set(Node(1)));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [1]);
    update(&mut app)
        .configure_sets(Node(1).after(Node(0)))
        .add_systems(mark(0).in_set(Node(0)));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [0, 1]);
}

#[test]
fn stages_run_in_update_extract_render_order_each_tick() {
    let mut app = tracing_app();
    app.schedule_mut(Stage::Startup).add_systems(mark(10));
    update(&mut app).add_systems(mark(20));
    app.schedule_mut(Stage::Extract).add_systems(mark(30));
    app.schedule_mut(Stage::Shutdown).add_systems(mark(50));
    // Render runs in the render world, so record through that world's resource.
    app.render_world_mut().init_resource::<Trace>();
    app.schedule_mut(Stage::Render)
        .add_systems(|t: Res<Trace>| t.push(40));
    app.run_fixed(2, DT).unwrap();
    assert_eq!(trace_of(&mut app), [10, 20, 30, 20, 30, 50]);
    assert_eq!(app.render_world().resource::<Trace>().take(), [40, 40]);
}

// ---- random dependency graphs --------------------------------------------

fn random_dag_run(seed: u64, nodes: u32, edge_percent: u64, shuffle_registration: bool) {
    let mut rng = Rng::new(seed);
    let mut edges = Vec::new();
    for from in 0..nodes {
        for to in (from + 1)..nodes {
            if rng.chance(edge_percent, 100) {
                edges.push((from, to));
            }
        }
    }
    let mut order: Vec<u32> = (0..nodes).collect();
    if shuffle_registration {
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i as u64 + 1) as usize);
        }
    }
    let mut app = tracing_app();
    for id in &order {
        update(&mut app).add_systems(mark(*id).in_set(Node(*id)));
    }
    let mut edge_order = edges.clone();
    if shuffle_registration {
        for i in (1..edge_order.len()).rev() {
            edge_order.swap(i, rng.below(i as u64 + 1) as usize);
        }
    }
    for (from, to) in &edge_order {
        update(&mut app).configure_sets(Node(*from).before(Node(*to)));
    }
    for tick in 0..4 {
        app.tick(DT).unwrap();
        let trace = trace_of(&mut app);
        assert_eq!(
            trace.len(),
            nodes as usize,
            "tick {tick}: every node runs once"
        );
        let mut sorted = trace.clone();
        sorted.sort();
        assert_eq!(sorted, (0..nodes).collect::<Vec<_>>());
        for (from, to) in &edges {
            assert!(
                position(&trace, *from) < position(&trace, *to),
                "seed {seed}: {from} must precede {to} in {trace:?}"
            );
        }
    }
}

macro_rules! dag_cases {
    ($($name:ident: $seed:expr, $nodes:expr, $percent:expr, $shuffle:expr;)*) => {
        $( #[test] fn $name() { random_dag_run($seed, $nodes, $percent, $shuffle); } )*
    };
}

dag_cases! {
    dag_sparse_small_in_registration_order: 11, 6, 25, false;
    dag_sparse_small_shuffled: 12, 6, 25, true;
    dag_dense_small_shuffled: 13, 8, 80, true;
    dag_medium_even_density: 14, 20, 30, true;
    dag_medium_in_registration_order: 15, 20, 30, false;
    dag_wide_and_nearly_unconstrained: 16, 40, 3, true;
    dag_deep_and_almost_total_order: 17, 24, 95, true;
    dag_large_sparse: 18, 64, 8, true;
    dag_single_node: 19, 1, 50, false;
    dag_two_nodes_forced_edge: 20, 2, 100, true;
}

// ---- conflicts and ambiguity ---------------------------------------------

fn expect_invalid_update(app: &mut EngineApp) -> String {
    let error = app.tick(DT).unwrap_err().to_string();
    assert!(error.contains("Update schedule is invalid"), "{error}");
    assert_eq!(app.state(), AppState::Failed);
    error
}

#[test]
fn unordered_writers_of_the_same_resource_are_rejected_before_running() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut c: ResMut<Counter>| c.0 += 2,
    ));
    expect_invalid_update(&mut app);
    assert_eq!(app.world().resource::<Counter>().0, 0);
}

#[test]
fn unordered_reader_and_writer_of_a_resource_are_rejected() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |c: Res<Counter>| {
            let _ = c.0;
        },
    ));
    expect_invalid_update(&mut app);
}

#[test]
fn unordered_readers_of_the_same_resource_are_accepted() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        |c: Res<Counter>, t: Res<Trace>| t.push(c.0 as u32 + 1),
        |c: Res<Counter>, t: Res<Trace>| t.push(c.0 as u32 + 2),
    ));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app).len(), 2);
}

#[test]
fn ordering_resolves_a_resource_conflict() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems(
        (
            |mut c: ResMut<Counter>| c.0 = c.0 * 10 + 1,
            |mut c: ResMut<Counter>| c.0 = c.0 * 10 + 2,
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 12);
}

#[test]
fn writers_to_different_resources_do_not_conflict() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut s: ResMut<Seen>| s.0.push(1),
    ));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 1);
}

#[test]
fn overlapping_component_writers_are_rejected() {
    let mut app = tracing_app();
    update(&mut app).add_systems((
        |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 += 1),
        |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 *= 2),
    ));
    expect_invalid_update(&mut app);
}

#[test]
fn writers_to_different_components_are_accepted() {
    let mut app = tracing_app();
    app.world_mut().spawn((Health(1), Pos::default()));
    update(&mut app).add_systems((
        |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 += 1),
        |mut q: Query<&mut Pos>| q.iter_mut().for_each(|mut p| p.x += 1),
    ));
    app.tick(DT).unwrap();
    let (h, p) = app
        .world_mut()
        .query::<(&Health, &Pos)>()
        .iter(app.world())
        .map(|(h, p)| (h.0, p.x))
        .next()
        .unwrap();
    assert_eq!((h, p), (2, 1));
}

#[test]
fn component_reader_and_writer_conflict_unless_ordered() {
    let mut unordered = tracing_app();
    update(&mut unordered).add_systems((
        |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 += 1),
        |q: Query<&Health>| {
            let _ = q.iter().count();
        },
    ));
    expect_invalid_update(&mut unordered);

    let mut ordered = tracing_app();
    update(&mut ordered).add_systems(
        (
            |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 += 1),
            |q: Query<&Health>| {
                let _ = q.iter().count();
            },
        )
            .chain(),
    );
    ordered.tick(DT).unwrap();
}

#[test]
fn filter_disjoint_writers_to_the_same_component_are_not_in_conflict() {
    let mut app = tracing_app();
    app.world_mut().spawn((Health(1), Tag));
    app.world_mut().spawn(Health(10));
    update(&mut app).add_systems((
        |mut q: Query<&mut Health, With<Tag>>| q.iter_mut().for_each(|mut h| h.0 += 1),
        |mut q: Query<&mut Health, Without<Tag>>| q.iter_mut().for_each(|mut h| h.0 += 2),
    ));
    app.tick(DT).unwrap();
    let mut values: Vec<_> = app
        .world_mut()
        .query::<&Health>()
        .iter(app.world())
        .map(|h| h.0)
        .collect();
    values.sort();
    assert_eq!(values, [2, 12]);
}

#[test]
fn exclusive_systems_conflict_with_everything_unless_ordered() {
    let mut app = tracing_app();
    update(&mut app).add_systems((
        |_: &mut World| {},
        |q: Query<&Health>| {
            let _ = q.iter().count();
        },
    ));
    expect_invalid_update(&mut app);
}

#[test]
fn ambiguous_with_explicitly_allows_unordered_conflicts() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        (|mut c: ResMut<Counter>| c.0 += 1)
            .in_set(Node(1))
            .ambiguous_with(Node(2)),
        (|mut c: ResMut<Counter>| c.0 += 1).in_set(Node(2)),
    ));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn ambiguity_detection_can_be_relaxed_by_the_caller() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Ignore,
        ..Default::default()
    });
    update(&mut app).add_systems((
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut c: ResMut<Counter>| c.0 += 1,
    ));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn conflict_diagnostics_do_not_run_any_system_in_the_stage() {
    let mut app = tracing_app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        mark(1),
        |mut c: ResMut<Counter>| c.0 += 1,
        |mut c: ResMut<Counter>| c.0 += 1,
    ));
    expect_invalid_update(&mut app);
    assert!(trace_of(&mut app).is_empty());
}

#[test]
fn conflicts_are_detected_in_every_stage() {
    for stage in [Stage::Startup, Stage::Extract, Stage::Shutdown] {
        let mut app = tracing_app();
        app.world_mut().init_resource::<Counter>();
        app.schedule_mut(stage).add_systems((
            |mut c: ResMut<Counter>| c.0 += 1,
            |mut c: ResMut<Counter>| c.0 += 1,
        ));
        let error = match stage {
            Stage::Shutdown => {
                app.startup().unwrap();
                app.shutdown().unwrap_err().to_string()
            }
            _ => app.tick(DT).unwrap_err().to_string(),
        };
        assert!(
            error.contains(&format!("{stage:?} schedule is invalid")),
            "{error}"
        );
    }
}

// ---- cycles and invalid graphs --------------------------------------------

#[test]
fn two_node_set_cycle_is_rejected() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((Node(1).before(Node(2)), Node(2).before(Node(1))))
        .add_systems((mark(1).in_set(Node(1)), mark(2).in_set(Node(2))));
    expect_invalid_update(&mut app);
    assert!(trace_of(&mut app).is_empty());
}

#[test]
fn three_node_set_cycle_is_rejected() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((
            Node(1).before(Node(2)),
            Node(2).before(Node(3)),
            Node(3).before(Node(1)),
        ))
        .add_systems((
            mark(1).in_set(Node(1)),
            mark(2).in_set(Node(2)),
            mark(3).in_set(Node(3)),
        ));
    expect_invalid_update(&mut app);
}

#[test]
fn long_ring_of_fifty_sets_is_rejected() {
    let mut app = tracing_app();
    for id in 0..50_u32 {
        update(&mut app)
            .configure_sets(Node(id).before(Node((id + 1) % 50)))
            .add_systems(mark(id).in_set(Node(id)));
    }
    expect_invalid_update(&mut app);
}

#[test]
fn cycle_mixing_chain_and_explicit_back_edge_is_rejected() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets((Phase::Input, Phase::Simulate, Phase::Resolve).chain())
        .configure_sets(Phase::Input.after(Phase::Resolve))
        .add_systems((
            mark(1).in_set(Phase::Input),
            mark(2).in_set(Phase::Simulate),
            mark(3).in_set(Phase::Resolve),
        ));
    expect_invalid_update(&mut app);
}

#[test]
fn system_cycle_through_function_labels_is_rejected() {
    fn a(t: Res<Trace>) {
        t.push(1);
    }
    fn b(t: Res<Trace>) {
        t.push(2);
    }
    let mut app = tracing_app();
    update(&mut app).add_systems((a.before(b), b.before(a)));
    expect_invalid_update(&mut app);
}

#[test]
fn ordering_a_system_relative_to_a_set_it_belongs_to_is_rejected() {
    let mut app = tracing_app();
    update(&mut app).add_systems(mark(1).in_set(Node(1)).before(Node(1)));
    expect_invalid_update(&mut app);
}

#[test]
fn ordered_sets_that_share_a_system_are_rejected() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets(Node(1).before(Node(2)))
        .add_systems(mark(1).in_set(Node(1)).in_set(Node(2)));
    expect_invalid_update(&mut app);
}

#[test]
fn a_set_cannot_contain_itself_through_its_parent() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets(Node(1).in_set(Node(2)))
        .configure_sets(Node(2).in_set(Node(1)))
        .add_systems(mark(1).in_set(Node(1)));
    expect_invalid_update(&mut app);
}

#[test]
fn failed_stage_marks_app_failed_and_it_cannot_tick_again() {
    let mut app = tracing_app();
    update(&mut app).configure_sets((Node(1).before(Node(2)), Node(2).before(Node(1))));
    update(&mut app).add_systems((mark(1).in_set(Node(1)), mark(2).in_set(Node(2))));
    expect_invalid_update(&mut app);
    let again = app.tick(DT).unwrap_err().to_string();
    assert!(
        again.contains("cannot start app in state Failed"),
        "{again}"
    );
}

#[test]
fn references_to_unpopulated_sets_are_accepted_and_vacuous() {
    // Documented Bevy limitation: no diagnostic for an empty label.
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets(Node(2).after(Node(99)))
        .add_systems(mark(2).in_set(Node(2)));
    app.tick(DT).unwrap();
    assert_eq!(trace_of(&mut app), [2]);
}

#[test]
fn schedule_initialize_reports_build_metadata_once() {
    let mut world = World::new();
    let mut schedule = Schedule::default();
    schedule.add_systems((|| {}, || {}));
    assert!(schedule.initialize(&mut world).unwrap().is_some());
    assert!(schedule.initialize(&mut world).unwrap().is_none());
    schedule.add_systems(|| {});
    assert!(schedule.initialize(&mut world).unwrap().is_some());
}

#[test]
fn schedule_initialize_returns_an_error_for_a_cycle() {
    let mut world = World::new();
    let mut schedule = Schedule::default();
    schedule
        .configure_sets((Node(1).before(Node(2)), Node(2).before(Node(1))))
        .add_systems((|| {}).in_set(Node(1)));
    schedule.add_systems((|| {}).in_set(Node(2)));
    assert!(schedule.initialize(&mut world).is_err());
}

// ---- run conditions and piping ----------------------------------------------

#[test]
fn run_if_skips_a_system_without_affecting_its_neighbours() {
    let mut app = tracing_app();
    update(&mut app).add_systems(
        (
            mark(1),
            mark(2).run_if(|time: Res<Time>| time.tick % 2 == 0),
            mark(3),
        )
            .chain(),
    );
    app.run_fixed(4, DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 3, 1, 2, 3, 1, 3, 1, 2, 3]);
}

#[test]
fn multiple_run_if_conditions_are_anded() {
    let mut app = tracing_app();
    update(&mut app).add_systems(
        mark(1)
            .run_if(|time: Res<Time>| time.tick > 2)
            .run_if(|time: Res<Time>| time.tick < 5),
    );
    app.run_fixed(8, DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 1]);
}

#[test]
fn condition_or_combinator_passes_when_either_side_holds() {
    let mut app = tracing_app();
    update(&mut app).add_systems(
        mark(1).run_if((|t: Res<Time>| t.tick == 1).or_else(|t: Res<Time>| t.tick == 4)),
    );
    app.run_fixed(6, DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 1]);
}

fn evaluations(eager: bool) -> usize {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let probe = Arc::new(AtomicUsize::new(0));
    let second = Arc::clone(&probe);
    let always = |_: Res<Time>| true;
    let counted = move |_: Res<Time>| {
        second.fetch_add(1, Ordering::SeqCst);
        true
    };
    let mut app = tracing_app();
    if eager {
        update(&mut app).add_systems(mark(1).run_if(always.or_eager(counted)));
    } else {
        update(&mut app).add_systems(mark(1).run_if(always.or_else(counted)));
    }
    app.run_fixed(5, DT).unwrap();
    probe.load(Ordering::SeqCst)
}

#[test]
fn or_else_short_circuits_the_second_condition() {
    assert_eq!(evaluations(false), 0);
}

#[test]
fn or_eager_always_evaluates_both_conditions() {
    assert_eq!(evaluations(true), 5);
}

#[test]
fn set_level_run_if_gates_every_member() {
    let mut app = tracing_app();
    update(&mut app)
        .configure_sets(Node(1).run_if(|t: Res<Time>| t.tick >= 3))
        .add_systems((mark(1).in_set(Node(1)), mark(2).in_set(Node(1)), mark(3)));
    app.run_fixed(3, DT).unwrap();
    let mut trace = trace_of(&mut app);
    trace.sort();
    assert_eq!(trace, [1, 2, 3, 3, 3]);
}

#[test]
fn conditions_see_commands_applied_by_earlier_ordered_systems() {
    let mut app = tracing_app();
    update(&mut app).add_systems(
        (
            |mut c: Commands, time: Res<Time>| {
                if time.tick == 2 {
                    c.spawn(A);
                }
            },
            mark(1).run_if(|q: Query<(), With<A>>| !q.is_empty()),
        )
            .chain(),
    );
    app.run_fixed(3, DT).unwrap();
    assert_eq!(trace_of(&mut app), [1, 1]);
}

#[test]
fn piped_systems_pass_values_between_stages() {
    let mut app = tracing_app();
    update(&mut app).add_systems(
        (|time: Res<Time>| time.tick as u32 * 7).pipe(|In(v): In<u32>, t: Res<Trace>| t.push(v)),
    );
    app.run_fixed(3, DT).unwrap();
    assert_eq!(trace_of(&mut app), [7, 14, 21]);
}

#[test]
fn each_conflict_free_system_pair_in_a_grid_of_components_is_accepted() {
    #[derive(Component)]
    struct C0(u32);
    #[derive(Component)]
    struct C1(u32);
    #[derive(Component)]
    struct C2(u32);
    #[derive(Component)]
    struct C3(u32);
    let mut app = tracing_app();
    app.world_mut().spawn((C0(0), C1(0), C2(0), C3(0)));
    update(&mut app).add_systems((
        |mut q: Query<&mut C0>| q.iter_mut().for_each(|mut c| c.0 += 1),
        |mut q: Query<&mut C1>| q.iter_mut().for_each(|mut c| c.0 += 2),
        |mut q: Query<&mut C2>| q.iter_mut().for_each(|mut c| c.0 += 3),
        |mut q: Query<&mut C3>| q.iter_mut().for_each(|mut c| c.0 += 4),
    ));
    app.run_fixed(10, DT).unwrap();
    let row = app
        .world_mut()
        .query::<(&C0, &C1, &C2, &C3)>()
        .iter(app.world())
        .map(|r| (r.0.0, r.1.0, r.2.0, r.3.0))
        .next()
        .unwrap();
    assert_eq!(row, (10, 20, 30, 40));
}
