//! Query data, include/exclude filters and change detection.
mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};

use common::*;
use engine_core::bevy_ecs::entity_disabling::Disabled;
use engine_core::bevy_ecs::query::{Allow, QueryEntityError};
use engine_core::bevy_ecs::resource::IsResource;
use engine_core::bevy_ecs::system::SystemState;
use engine_core::prelude::*;

#[derive(Component, Debug, Clone, PartialEq)]
struct A(i32);
#[derive(Component, Debug, Clone, PartialEq)]
struct B(i32);
#[derive(Component, Debug, Clone, PartialEq)]
struct C(i32);

#[derive(Component, Debug, PartialEq)]
#[component(storage = "SparseSet")]
struct Sparse(i32);

/// Eight entities, one per A/B/C membership mask (bit0=A, bit1=B, bit2=C).
fn mask_world() -> World {
    let mut world = World::new();
    for mask in 0..8_i32 {
        let mut e = world.spawn(Health(mask));
        if mask & 1 != 0 {
            e.insert(A(mask));
        }
        if mask & 2 != 0 {
            e.insert(B(mask));
        }
        if mask & 4 != 0 {
            e.insert(C(mask));
        }
    }
    world
}

macro_rules! filter_cases {
    ($($name:ident: $filter:ty => $expected:expr;)*) => {
        $(
            #[test]
            fn $name() {
                let mut world = mask_world();
                assert_eq!(count_with::<($filter, Without<IsResource>)>(&mut world), $expected);
            }
        )*
    };
}

filter_cases! {
    with_a_matches_four_masks: With<A> => 4;
    with_b_matches_four_masks: With<B> => 4;
    with_c_matches_four_masks: With<C> => 4;
    without_a_matches_the_complement: Without<A> => 4;
    with_a_and_b_matches_two: (With<A>, With<B>) => 2;
    with_a_without_b_matches_two: (With<A>, Without<B>) => 2;
    with_a_without_b_and_c_matches_only_mask_one: (With<A>, Without<B>, Without<C>) => 1;
    without_all_matches_only_mask_zero: (Without<A>, Without<B>, Without<C>) => 1;
    with_all_three_matches_only_mask_seven: (With<A>, With<B>, With<C>) => 1;
    or_of_a_and_b_matches_six: Or<(With<A>, With<B>)> => 6;
    or_of_all_three_matches_seven: Or<(With<A>, With<B>, With<C>)> => 7;
    or_with_nested_and_matches_five: Or<(With<A>, (With<B>, With<C>))> => 5;
    or_combined_with_without_matches_three: (Or<(With<A>, With<B>)>, Without<C>) => 3;
    or_of_two_withouts_is_not_a_and_b: Or<(Without<A>, Without<B>)> => 6;
    or_mixing_without_and_with_matches_six: Or<(Without<A>, With<B>)> => 6;
    health_with_no_other_filter_matches_all_eight: With<Health> => 8;
    without_health_matches_none: Without<Health> => 0;
    with_unregistered_component_matches_none: With<Sparse> => 0;
}

#[test]
fn resources_are_entities_that_unfiltered_entity_queries_can_see() {
    // Bevy 0.20 stores resources on entities marked `IsResource`; user queries that
    // exclude components (`Without<X>`) must also exclude `IsResource` to avoid them.
    let mut world = mask_world();
    world.insert_resource(Counter(0));
    let all = world.query::<Entity>().iter(&world).count();
    let user = world
        .query_filtered::<Entity, Without<IsResource>>()
        .iter(&world)
        .count();
    assert!(all > user);
    assert_eq!(user, 8);
    let resource_entities = world
        .query_filtered::<Entity, With<IsResource>>()
        .iter(&world)
        .count();
    assert_eq!(all - user, resource_entities);
}

#[test]
fn optional_data_yields_none_for_missing_components() {
    let mut world = mask_world();
    let somes = world
        .query::<Option<&A>>()
        .iter(&world)
        .filter(|a| a.is_some())
        .count();
    assert_eq!(somes, 4);
}

#[test]
fn optional_data_can_still_be_read_for_present_components() {
    let mut world = mask_world();
    let sum: i32 = world
        .query::<Option<&A>>()
        .iter(&world)
        .flatten()
        .map(|a| a.0)
        .sum();
    assert_eq!(sum, 1 + 3 + 5 + 7);
}

#[test]
fn has_reports_presence_without_requiring_it() {
    let mut world = mask_world();
    let present: usize = world
        .query::<(&Health, Has<B>)>()
        .iter(&world)
        .filter(|(_, has)| *has)
        .count();
    let total = world.query::<(&Health, Has<B>)>().iter(&world).count();
    assert_eq!((present, total), (4, 8));
}

#[test]
fn tuple_data_requires_all_members() {
    let mut world = mask_world();
    let rows: Vec<_> = world.query::<(&A, &B, &C)>().iter(&world).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].0.0, rows[0].1.0, rows[0].2.0), (7, 7, 7));
}

#[test]
fn entity_in_query_data_matches_component_owner() {
    let mut world = mask_world();
    let rows: Vec<(Entity, i32)> = world
        .query::<(Entity, &Health)>()
        .iter(&world)
        .map(|(e, h)| (e, h.0))
        .collect();
    for (e, h) in rows {
        assert_eq!(world.get::<Health>(e).unwrap().0, h);
    }
}

#[test]
fn mutable_query_updates_are_visible_afterwards() {
    let mut world = mask_world();
    for mut a in world.query::<&mut A>().iter_mut(&mut world) {
        a.0 *= 10;
    }
    let sum: i32 = world.query::<&A>().iter(&world).map(|a| a.0).sum();
    assert_eq!(sum, 160);
}

#[test]
fn iteration_visits_every_match_exactly_once() {
    let mut world = World::new();
    let mut expected = HashSet::new();
    for i in 0..30 {
        let e = match i % 3 {
            0 => world.spawn((A(i), Health(0))).id(),
            1 => world.spawn((A(i), B(0))).id(),
            _ => world.spawn((A(i), C(0), Tag)).id(),
        };
        expected.insert(e);
    }
    let seen: Vec<Entity> = world
        .query_filtered::<Entity, With<A>>()
        .iter(&world)
        .collect();
    assert_eq!(seen.len(), 30);
    assert_eq!(seen.into_iter().collect::<HashSet<_>>(), expected);
}

#[test]
fn single_archetype_iteration_follows_spawn_order() {
    let mut world = World::new();
    let ids: Vec<_> = (0..16).map(|i| world.spawn(A(i)).id()).collect();
    let seen: Vec<_> = world
        .query::<(Entity, &A)>()
        .iter(&world)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(seen, ids);
}

#[test]
fn query_get_reports_missing_entity_and_missing_component_differently() {
    let mut world = World::new();
    let has = world.spawn(A(1)).id();
    let lacks = world.spawn(B(1)).id();
    let dead = world.spawn(A(2)).id();
    world.despawn(dead);
    let mut state = world.query::<&A>();
    assert_eq!(state.get(&world, has).map(|a| a.0), Ok(1));
    assert!(matches!(
        state.get(&world, lacks),
        Err(QueryEntityError::QueryDoesNotMatch(..))
    ));
    assert!(matches!(
        state.get(&world, dead),
        Err(QueryEntityError::NotSpawned(..))
    ));
}

#[test]
fn query_get_mut_modifies_only_the_requested_entity() {
    let mut world = World::new();
    let a = world.spawn(A(1)).id();
    let b = world.spawn(A(2)).id();
    let mut state = world.query::<&mut A>();
    state.get_mut(&mut world, a).unwrap().0 = 100;
    assert_eq!(world.get::<A>(a), Some(&A(100)));
    assert_eq!(world.get::<A>(b), Some(&A(2)));
}

#[test]
fn get_many_returns_values_in_requested_order() {
    let mut world = World::new();
    let ids: Vec<_> = (0..4).map(|i| world.spawn(A(i * 5)).id()).collect();
    let mut state = world.query::<&A>();
    let got = state.get_many(&world, [ids[3], ids[0], ids[2]]).unwrap();
    assert_eq!(got.map(|a| a.0), [15, 0, 10]);
}

#[test]
fn get_many_fails_when_any_entity_does_not_match() {
    let mut world = World::new();
    let a = world.spawn(A(1)).id();
    let b = world.spawn(B(1)).id();
    let mut state = world.query::<&A>();
    assert!(state.get_many(&world, [a, b]).is_err());
}

#[test]
fn get_many_mut_rejects_duplicate_entities() {
    let mut world = World::new();
    let a = world.spawn(A(1)).id();
    let b = world.spawn(A(2)).id();
    let mut state = world.query::<&mut A>();
    assert!(matches!(
        state.get_many_mut(&mut world, [a, a]),
        Err(QueryEntityError::AliasedMutability(..))
    ));
    let [mut x, mut y] = state.get_many_mut(&mut world, [a, b]).unwrap();
    std::mem::swap(&mut x.0, &mut y.0);
    assert_eq!(world.get::<A>(a), Some(&A(2)));
    assert_eq!(world.get::<A>(b), Some(&A(1)));
}

#[test]
fn iter_many_reports_non_matching_entities_inline_and_keeps_order() {
    let mut world = World::new();
    let a1 = world.spawn(A(1)).id();
    let b = world.spawn(B(9)).id();
    let a2 = world.spawn(A(2)).id();
    let mut state = world.query::<&A>();
    let rows: Vec<_> = state.iter_many(&world, [a2, b, a1]).collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].as_ref().map(|a| a.0).ok(), Some(2));
    assert!(rows[1].is_err());
    assert_eq!(rows[2].as_ref().map(|a| a.0).ok(), Some(1));
}

#[test]
fn iter_combinations_produces_unordered_pairs() {
    let mut world = World::new();
    for i in 0..6 {
        world.spawn(A(i));
    }
    let mut state = world.query::<&A>();
    let pairs: Vec<[i32; 2]> = state
        .iter_combinations::<2>(&world)
        .map(|[x, y]| [x.0, y.0])
        .collect();
    assert_eq!(pairs.len(), 15);
    assert!(pairs.iter().all(|[x, y]| x != y));
    let unordered: HashSet<_> = pairs.iter().map(|[x, y]| (*x.min(y), *x.max(y))).collect();
    assert_eq!(unordered.len(), 15);
}

#[test]
fn single_succeeds_for_exactly_one_match() {
    let mut world = World::new();
    world.spawn(A(1));
    let mut state = world.query::<&A>();
    assert_eq!(state.single(&world).map(|a| a.0).ok(), Some(1));
}

#[test]
fn single_reports_no_entities_and_multiple_entities() {
    let mut world = World::new();
    let mut state = world.query::<&A>();
    assert!(state.single(&world).is_err());
    world.spawn(A(1));
    world.spawn(A(2));
    assert!(state.single(&world).is_err());
}

#[test]
fn system_state_query_matches_world_query() {
    let mut world = mask_world();
    let mut state: SystemState<Query<&A>> = SystemState::new(&mut world);
    let query = state.get(&world).unwrap();
    assert_eq!(query.iter().count(), 4);
    assert_eq!(query.iter().map(|a| a.0).sum::<i32>(), 16);
}

#[test]
fn query_is_empty_and_contains_reflect_membership() {
    let mut world = World::new();
    let a = world.spawn(A(1)).id();
    let b = world.spawn(B(1)).id();
    let mut state: SystemState<Query<&A>> = SystemState::new(&mut world);
    let q = state.get(&world).unwrap();
    assert!(!q.is_empty());
    assert!(q.contains(a));
    assert!(!q.contains(b));
}

#[test]
fn sparse_set_components_are_queryable_and_filterable() {
    let mut world = World::new();
    let with = world.spawn((A(1), Sparse(5))).id();
    world.spawn(A(2));
    let mut state = world.query_filtered::<Entity, With<Sparse>>();
    assert_eq!(state.iter(&world).collect::<Vec<_>>(), [with]);
    let mut without = world.query_filtered::<&A, Without<Sparse>>();
    assert_eq!(without.iter(&world).map(|a| a.0).collect::<Vec<_>>(), [2]);
}

#[test]
fn disabled_entities_are_hidden_from_default_queries() {
    let mut world = World::new();
    world.spawn(A(1));
    let hidden = world.spawn((A(2), Disabled)).id();
    assert_eq!(world.query::<&A>().iter(&world).count(), 1);
    assert_eq!(world.get::<A>(hidden), Some(&A(2)));
}

#[test]
fn disabled_entities_appear_when_query_mentions_them() {
    let mut world = World::new();
    world.spawn(A(1));
    world.spawn((A(2), Disabled));
    assert_eq!(
        world
            .query_filtered::<&A, With<Disabled>>()
            .iter(&world)
            .count(),
        1
    );
    assert_eq!(
        world
            .query_filtered::<&A, Allow<Disabled>>()
            .iter(&world)
            .count(),
        2
    );
}

#[test]
fn enabling_an_entity_makes_it_visible_again() {
    let mut world = World::new();
    let e = world.spawn((A(1), Disabled)).id();
    assert_eq!(world.query::<&A>().iter(&world).count(), 0);
    world.entity_mut(e).remove::<Disabled>();
    assert_eq!(world.query::<&A>().iter(&world).count(), 1);
}

#[test]
fn par_iter_visits_each_entity_once() {
    let mut world = World::new();
    for i in 1..=500 {
        world.spawn(A(i));
    }
    let total = AtomicI64::new(0);
    let visits = AtomicI64::new(0);
    let mut state: SystemState<Query<&A>> = SystemState::new(&mut world);
    state.get(&world).unwrap().par_iter().for_each(|a| {
        total.fetch_add(i64::from(a.0), Ordering::SeqCst);
        visits.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(visits.load(Ordering::SeqCst), 500);
    assert_eq!(total.load(Ordering::SeqCst), 500 * 501 / 2);
}

// ---- system-level behaviour -------------------------------------------------

#[test]
fn disjoint_mutable_queries_via_without_coexist_in_one_system() {
    let mut app = app();
    app.world_mut().spawn((A(1), Tag));
    app.world_mut().spawn(A(10));
    update(&mut app).add_systems(
        |mut tagged: Query<&mut A, With<Tag>>, mut plain: Query<&mut A, Without<Tag>>| {
            for mut a in &mut tagged {
                a.0 += 1;
            }
            for mut a in &mut plain {
                a.0 += 100;
            }
        },
    );
    app.tick(DT).unwrap();
    let mut values: Vec<_> = app
        .world_mut()
        .query::<&A>()
        .iter(app.world())
        .map(|a| a.0)
        .collect();
    values.sort();
    assert_eq!(values, [2, 110]);
}

#[test]
#[should_panic]
fn overlapping_mutable_and_shared_queries_in_one_system_panic_at_initialisation() {
    let mut world = World::new();
    let mut schedule = Schedule::default();
    schedule.add_systems(|_a: Query<&mut A>, _b: Query<&A>| {});
    let _ = schedule.initialize(&mut world);
    schedule.run(&mut world);
}

#[test]
fn param_set_allows_sequential_conflicting_queries() {
    let mut app = app();
    app.world_mut().spawn(A(1));
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(
        |mut set: ParamSet<(Query<&mut A>, Query<&A>)>, mut seen: ResMut<Seen>| {
            for mut a in set.p0().iter_mut() {
                a.0 += 5;
            }
            seen.0.extend(set.p1().iter().map(|a| i64::from(a.0)));
        },
    );
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [6]);
}

#[test]
fn single_param_skips_systems_unless_exactly_one_match_exists() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems(|a: Single<&A>, mut c: ResMut<Counter>| c.0 += i64::from(a.0));
    app.tick(DT).unwrap(); // zero matches: skipped
    assert_eq!(app.world().resource::<Counter>().0, 0);
    app.world_mut().spawn(A(5));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 5);
    app.world_mut().spawn(A(7));
    app.tick(DT).unwrap(); // two matches: skipped again
    assert_eq!(app.world().resource::<Counter>().0, 5);
}

#[test]
fn populated_param_skips_systems_on_empty_queries() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app)
        .add_systems(|a: Populated<&A>, mut c: ResMut<Counter>| c.0 += a.iter().count() as i64);
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 0);
    for i in 0..3 {
        app.world_mut().spawn(A(i));
    }
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 3);
}

#[test]
fn queries_observe_archetypes_created_after_the_system_was_registered() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|q: Query<&A>, mut seen: ResMut<Seen>| {
        seen.0.push(q.iter().count() as i64);
    });
    app.tick(DT).unwrap();
    app.world_mut().spawn(A(1));
    app.tick(DT).unwrap();
    app.world_mut().spawn((A(2), B(2)));
    app.tick(DT).unwrap();
    app.world_mut().spawn((A(3), B(3), C(3)));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0, 1, 2, 3]);
}

#[test]
fn entities_that_gain_a_component_join_matching_queries_next_run() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(B(0)).id();
    update(&mut app).add_systems(|q: Query<&A>, mut seen: ResMut<Seen>| {
        seen.0.push(q.iter().count() as i64);
    });
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(e).insert(A(1));
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(e).remove::<A>();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0, 1, 0]);
}

// ---- change detection -------------------------------------------------------

type EitherChanged = Or<(Changed<A>, Changed<B>)>;

#[derive(Resource, Default)]
struct Plan(Vec<u8>);

fn writer(time: Res<Time>, plan: Res<Plan>, mut query: Query<&mut A>) {
    let Some(op) = plan.0.get(time.tick.saturating_sub(2) as usize).copied() else {
        return;
    };
    if time.tick < 2 {
        return;
    }
    for mut a in &mut query {
        match op {
            0 => {}
            1 => a.0 += 1,
            2 => {
                let _ = &*a; // shared deref of a Mut must not flag a change
            }
            3 => {
                let same = a.0;
                a.set_if_neq(A(same));
            }
            4 => {
                let next = a.0 + 1;
                a.set_if_neq(A(next));
            }
            5 => a.bypass_change_detection().0 += 1,
            6 => a.set_changed(),
            7 => *a = A(a.0),
            _ => unreachable!(),
        }
    }
}

fn observer(query: Query<(), Changed<A>>, mut seen: ResMut<Seen>) {
    seen.0.push(query.iter().count() as i64);
}

fn run_plan(plan: &[u8]) -> Vec<i64> {
    let mut app = app();
    app.world_mut().spawn(A(0));
    app.world_mut().insert_resource(Plan(plan.to_vec()));
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems((writer, observer).chain());
    for _ in 0..=plan.len() {
        app.tick(DT).unwrap();
    }
    app.world().resource::<Seen>().0.clone()
}

macro_rules! change_cases {
    ($($name:ident: $plan:expr => $expected:expr;)*) => {
        $( #[test] fn $name() { assert_eq!(run_plan(&$plan), $expected); } )*
    };
}

change_cases! {
    spawn_counts_as_a_change_on_the_first_run_only: [0, 0, 0] => vec![1, 0, 0, 0];
    deref_mut_write_is_a_change: [1] => vec![1, 1];
    shared_deref_of_mut_is_not_a_change: [2] => vec![1, 0];
    set_if_neq_with_equal_value_is_not_a_change: [3] => vec![1, 0];
    set_if_neq_with_new_value_is_a_change: [4] => vec![1, 1];
    bypass_change_detection_is_not_a_change: [5] => vec![1, 0];
    set_changed_forces_a_change: [6] => vec![1, 1];
    whole_value_overwrite_with_same_value_is_still_a_change: [7] => vec![1, 1];
    alternating_writes_and_idle_ticks_track_exactly: [1, 0, 1, 0, 0, 1] => vec![1, 1, 0, 1, 0, 0, 1];
    consecutive_writes_report_each_tick: [1, 1, 1, 1] => vec![1, 1, 1, 1, 1];
    mixed_silent_and_flagging_ops: [5, 2, 3, 4, 6, 0] => vec![1, 0, 0, 0, 1, 1, 0];
}

fn tracked_observations(add_first: bool) -> (Vec<i64>, Vec<i64>) {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.world_mut().insert_resource(AddedSeen::default());
    let e = if add_first {
        Some(app.world_mut().spawn(A(1)).id())
    } else {
        None
    };
    update(&mut app).add_systems(
        |added: Query<(), Added<A>>,
         changed: Query<(), Changed<A>>,
         mut a: ResMut<AddedSeen>,
         mut c: ResMut<Seen>| {
            a.0.push(added.iter().count() as i64);
            c.0.push(changed.iter().count() as i64);
        },
    );
    app.tick(DT).unwrap();
    let id = e.unwrap_or_else(|| app.world_mut().spawn_empty().id());
    app.world_mut().entity_mut(id).insert(A(5)); // new or replacement
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<A>(id).unwrap().0 = 6;
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(id).remove::<A>();
    app.world_mut().entity_mut(id).insert(A(7));
    app.tick(DT).unwrap();
    (
        app.world().resource::<AddedSeen>().0.clone(),
        app.world().resource::<Seen>().0.clone(),
    )
}

#[derive(Resource, Default)]
struct AddedSeen(Vec<i64>);

#[test]
fn added_fires_once_per_insertion_and_again_after_remove_and_reinsert() {
    let (added, changed) = tracked_observations(true);
    // tick1: spawned; tick2: replaced (changed, not added); tick3: mutated;
    // tick4: removed and re-added (added and changed).
    assert_eq!(added, [1, 0, 0, 1]);
    assert_eq!(changed, [1, 1, 1, 1]);
}

#[test]
fn inserting_a_brand_new_component_counts_as_added_and_changed() {
    let (added, changed) = tracked_observations(false);
    assert_eq!(added, [0, 1, 0, 1]);
    assert_eq!(changed, [0, 1, 1, 1]);
}

#[test]
fn ref_reports_added_and_changed_flags() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(A(1)).id();
    update(&mut app).add_systems(|q: Query<Ref<A>>, mut seen: ResMut<Seen>| {
        for a in &q {
            seen.0
                .push(i64::from(a.is_added()) * 10 + i64::from(a.is_changed()));
        }
    });
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<A>(e).unwrap().0 = 3;
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [11, 0, 1]);
}

#[test]
fn two_systems_each_observe_a_modification_exactly_once() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.world_mut().insert_resource(AddedSeen::default());
    let e = app.world_mut().spawn(A(1)).id();
    update(&mut app).add_systems((
        |q: Query<(), Changed<A>>, mut s: ResMut<Seen>| s.0.push(q.iter().count() as i64),
        |q: Query<(), Changed<A>>, mut s: ResMut<AddedSeen>| s.0.push(q.iter().count() as i64),
    ));
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<A>(e).unwrap().0 = 2;
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0, 1, 0]);
    assert_eq!(app.world().resource::<AddedSeen>().0, [1, 0, 1, 0]);
}

#[test]
fn a_system_that_skips_ticks_still_sees_changes_since_its_last_run() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(A(1)).id();
    update(&mut app).add_systems(
        (|q: Query<(), Changed<A>>, mut s: ResMut<Seen>| s.0.push(q.iter().count() as i64))
            .run_if(|time: Res<Time>| time.tick.is_multiple_of(3)),
    );
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<A>(e).unwrap().0 = 2;
    app.tick(DT).unwrap();
    app.tick(DT).unwrap(); // first real run: spawn + change both pending
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.tick(DT).unwrap(); // second run: nothing new since
    assert_eq!(app.world().resource::<Seen>().0, [1, 0]);
}

#[test]
fn changed_ignores_despawned_entities() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(A(1)).id();
    update(&mut app).add_systems(|q: Query<(), Changed<A>>, mut s: ResMut<Seen>| {
        s.0.push(q.iter().count() as i64);
    });
    app.world_mut().get_mut::<A>(e).unwrap().0 = 2;
    app.world_mut().despawn(e);
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0]);
}

#[test]
fn changed_in_or_filter_matches_any_changed_member() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn((A(1), B(1))).id();
    update(&mut app).add_systems(|q: Query<(), EitherChanged>, mut s: ResMut<Seen>| {
        s.0.push(q.iter().count() as i64);
    });
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<B>(e).unwrap().0 = 5;
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<A>(e).unwrap().0 = 5;
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0, 1, 1]);
}

#[test]
fn entity_spawned_by_commands_is_added_for_systems_ordered_after_application() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(
        (
            |mut commands: Commands, time: Res<Time>| {
                if time.tick == 1 {
                    commands.spawn(A(1));
                }
            },
            |q: Query<(), Added<A>>, mut s: ResMut<Seen>| s.0.push(q.iter().count() as i64),
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0]);
}

#[test]
fn entity_spawned_by_commands_reaches_earlier_unordered_system_only_next_tick() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(
        (
            |q: Query<(), Added<A>>, mut s: ResMut<Seen>| s.0.push(q.iter().count() as i64),
            |mut commands: Commands, time: Res<Time>| {
                if time.tick == 1 {
                    commands.spawn(A(1));
                }
            },
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0, 1]);
}

#[test]
fn many_entities_report_independent_change_flags() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let ids: Vec<_> = (0..10).map(|i| app.world_mut().spawn(A(i)).id()).collect();
    update(&mut app).add_systems(|q: Query<&A, Changed<A>>, mut s: ResMut<Seen>| {
        let mut values: Vec<i64> = q.iter().map(|a| i64::from(a.0)).collect();
        values.sort();
        s.0.push(values.iter().sum());
        s.0.push(values.len() as i64);
    });
    app.tick(DT).unwrap();
    for id in [ids[2], ids[5], ids[9]] {
        app.world_mut().get_mut::<A>(id).unwrap().0 += 100;
    }
    app.tick(DT).unwrap();
    assert_eq!(
        app.world().resource::<Seen>().0,
        [45, 10, 102 + 105 + 109, 3]
    );
}
