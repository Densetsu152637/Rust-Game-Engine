//! Component insertion, removal, replacement, bundles and archetype moves.
mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use common::*;
use engine_ecs::prelude::*;

#[derive(Component, Debug, Clone, PartialEq)]
struct A(i32);
#[derive(Component, Debug, Clone, PartialEq)]
struct B(i32);
#[derive(Component, Debug, Clone, PartialEq)]
struct C(i32);

#[derive(Component, Debug, PartialEq)]
#[component(storage = "SparseSet")]
struct Sparse(i32);

#[derive(Component, Debug, PartialEq)]
struct Wrapper<T: Send + Sync + 'static>(T);

#[derive(Component, Debug, PartialEq, Default)]
#[require(Health(100), Tag)]
struct Hero;

#[derive(Component, Debug, PartialEq)]
#[require(Hero)]
struct Champion;

#[derive(Bundle)]
struct AB {
    a: A,
    b: B,
}

#[derive(Bundle)]
struct Nested {
    ab: AB,
    c: C,
}

#[derive(Component)]
struct Droppy(Arc<AtomicUsize>);
impl Drop for Droppy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn insert_adds_a_component_to_a_live_entity() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    world.entity_mut(e).insert(A(4));
    assert_eq!(world.get::<A>(e), Some(&A(4)));
}

#[test]
fn insert_of_existing_type_replaces_value_in_place() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    world.entity_mut(e).insert(A(2));
    assert_eq!(world.get::<A>(e), Some(&A(2)));
    assert_eq!(count::<A>(&mut world), 1);
}

#[test]
fn insert_if_new_keeps_the_existing_value() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    world.entity_mut(e).insert_if_new((A(99), B(5)));
    assert_eq!(world.get::<A>(e), Some(&A(1)));
    assert_eq!(world.get::<B>(e), Some(&B(5)));
}

#[test]
fn remove_deletes_only_the_named_component() {
    let mut world = World::new();
    let e = world.spawn((A(1), B(2), C(3))).id();
    world.entity_mut(e).remove::<B>();
    assert_eq!(world.get::<A>(e), Some(&A(1)));
    assert!(world.get::<B>(e).is_none());
    assert_eq!(world.get::<C>(e), Some(&C(3)));
}

#[test]
fn removing_an_absent_component_is_a_no_op() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    world.entity_mut(e).remove::<B>();
    world.entity_mut(e).remove::<B>();
    assert_eq!(world.get::<A>(e), Some(&A(1)));
}

#[test]
fn take_returns_the_value_and_removes_it() {
    let mut world = World::new();
    let e = world.spawn((A(7), B(8))).id();
    assert_eq!(world.entity_mut(e).take::<A>(), Some(A(7)));
    assert!(world.get::<A>(e).is_none());
    assert_eq!(world.entity_mut(e).take::<A>(), None);
    assert_eq!(world.get::<B>(e), Some(&B(8)));
}

#[test]
fn contains_reflects_current_component_set() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    assert!(world.entity(e).contains::<A>());
    assert!(!world.entity(e).contains::<B>());
    world.entity_mut(e).insert(B(1));
    assert!(world.entity(e).contains::<B>());
}

#[test]
fn bundle_insert_adds_every_member() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    world.entity_mut(e).insert(AB { a: A(1), b: B(2) });
    assert_eq!(world.get::<A>(e), Some(&A(1)));
    assert_eq!(world.get::<B>(e), Some(&B(2)));
}

#[test]
fn bundle_remove_deletes_every_member() {
    let mut world = World::new();
    let e = world.spawn((AB { a: A(1), b: B(2) }, C(3))).id();
    world.entity_mut(e).remove::<AB>();
    assert!(world.get::<A>(e).is_none());
    assert!(world.get::<B>(e).is_none());
    assert_eq!(world.get::<C>(e), Some(&C(3)));
}

#[test]
fn bundle_remove_with_partially_present_members_removes_the_present_ones() {
    let mut world = World::new();
    let e = world.spawn((A(1), C(3))).id();
    world.entity_mut(e).remove::<AB>();
    assert!(world.get::<A>(e).is_none());
    assert_eq!(world.get::<C>(e), Some(&C(3)));
}

#[test]
fn nested_bundles_flatten_into_individual_components() {
    let mut world = World::new();
    let e = world
        .spawn(Nested {
            ab: AB { a: A(1), b: B(2) },
            c: C(3),
        })
        .id();
    assert_eq!(world.get::<A>(e), Some(&A(1)));
    assert_eq!(world.get::<B>(e), Some(&B(2)));
    assert_eq!(world.get::<C>(e), Some(&C(3)));
}

#[test]
fn creature_bundle_from_common_fixtures_spawns_all_parts() {
    let mut world = World::new();
    let e = world
        .spawn(Creature {
            body: Body::at(3, 4),
            health: Health(10),
        })
        .id();
    assert_eq!(world.get::<Pos>(e), Some(&Pos { x: 3, y: 4 }));
    assert_eq!(world.get::<Vel>(e), Some(&Vel::default()));
    assert_eq!(world.get::<Health>(e), Some(&Health(10)));
}

#[test]
fn tuple_bundles_and_named_bundles_produce_the_same_components() {
    let mut world = World::new();
    let named = world.spawn(AB { a: A(1), b: B(2) }).id();
    let tuple = world.spawn((A(1), B(2))).id();
    for e in [named, tuple] {
        assert_eq!(world.get::<A>(e), Some(&A(1)));
        assert_eq!(world.get::<B>(e), Some(&B(2)));
    }
    let loc_a = world.entity(named).archetype().id();
    let loc_b = world.entity(tuple).archetype().id();
    assert_eq!(loc_a, loc_b);
}

#[test]
fn retain_drops_everything_outside_the_bundle() {
    let mut world = World::new();
    let e = world.spawn((A(1), B(2), C(3))).id();
    world.entity_mut(e).retain::<(A, C)>();
    assert!(world.get::<A>(e).is_some());
    assert!(world.get::<B>(e).is_none());
    assert!(world.get::<C>(e).is_some());
}

#[test]
fn clear_removes_all_components_but_keeps_the_entity() {
    let mut world = World::new();
    let e = world.spawn((A(1), B(2), C(3))).id();
    world.entity_mut(e).clear();
    assert!(world.get_entity(e).is_ok());
    assert!(!world.entity(e).contains::<A>());
    assert!(!world.entity(e).contains::<B>());
    assert!(!world.entity(e).contains::<C>());
}

#[test]
fn same_layout_newtypes_are_distinct_components() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    assert!(world.get::<B>(e).is_none());
    world.entity_mut(e).insert(B(1));
    world.entity_mut(e).remove::<A>();
    assert!(world.get::<A>(e).is_none());
    assert_eq!(world.get::<B>(e), Some(&B(1)));
}

#[test]
fn generic_components_with_different_parameters_are_distinct() {
    let mut world = World::new();
    let e = world.spawn((Wrapper(1_i32), Wrapper(2.5_f32))).id();
    assert_eq!(world.get::<Wrapper<i32>>(e), Some(&Wrapper(1)));
    assert_eq!(world.get::<Wrapper<f32>>(e), Some(&Wrapper(2.5)));
    assert!(world.get::<Wrapper<u8>>(e).is_none());
    world.entity_mut(e).remove::<Wrapper<i32>>();
    assert_eq!(world.get::<Wrapper<f32>>(e), Some(&Wrapper(2.5)));
}

#[test]
fn zero_sized_markers_are_trackable_components() {
    let mut world = World::new();
    let tagged = world.spawn((A(1), Tag)).id();
    let plain = world.spawn(A(2)).id();
    assert!(world.entity(tagged).contains::<Tag>());
    assert!(!world.entity(plain).contains::<Tag>());
    assert_eq!(count::<Tag>(&mut world), 1);
}

#[test]
fn string_components_keep_independent_storage() {
    let mut world = World::new();
    let a = world.spawn(Label("alpha".into())).id();
    let b = world.spawn(Label("beta".into())).id();
    world.get_mut::<Label>(a).unwrap().0.push_str("-x");
    assert_eq!(world.get::<Label>(a).unwrap().0, "alpha-x");
    assert_eq!(world.get::<Label>(b).unwrap().0, "beta");
}

#[test]
fn get_mut_mutates_the_stored_value() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    world.get_mut::<A>(e).unwrap().0 += 41;
    assert_eq!(world.get::<A>(e), Some(&A(42)));
}

#[test]
fn archetype_changes_when_component_set_changes() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    let first = world.entity(e).archetype().id();
    world.entity_mut(e).insert(B(1));
    let second = world.entity(e).archetype().id();
    world.entity_mut(e).remove::<B>();
    let third = world.entity(e).archetype().id();
    assert_ne!(first, second);
    assert_eq!(
        first, third,
        "returning to the same set returns to the same archetype"
    );
}

#[test]
fn entities_with_identical_component_sets_share_an_archetype() {
    let mut world = World::new();
    let ids: Vec<_> = (0..20).map(|i| world.spawn((A(i), B(i))).id()).collect();
    let archetype = world.entity(ids[0]).archetype().id();
    for id in &ids {
        assert_eq!(world.entity(*id).archetype().id(), archetype);
    }
}

#[test]
fn archetype_count_grows_with_distinct_sets_not_entity_count() {
    let mut world = World::new();
    world.spawn(A(0));
    let before = world.archetypes().len();
    for i in 0..100 {
        world.spawn(A(i));
    }
    assert_eq!(world.archetypes().len(), before);
    world.spawn((A(0), B(0)));
    assert_eq!(world.archetypes().len(), before + 1);
}

#[test]
fn archetype_insertion_order_does_not_create_duplicate_archetypes() {
    let mut world = World::new();
    let x = world.spawn((A(1), B(2))).id();
    let y = world.spawn((B(2), A(1))).id();
    let z = world.spawn(A(1)).id();
    world.entity_mut(z).insert(B(2));
    let archetypes: HashSet<_> = [x, y, z]
        .iter()
        .map(|e| world.entity(*e).archetype().id())
        .collect();
    assert_eq!(archetypes.len(), 1);
}

#[test]
fn archetype_move_preserves_untouched_component_values() {
    let mut world = World::new();
    let e = world.spawn((A(10), B(20), C(30))).id();
    world.entity_mut(e).insert(Tag);
    world.entity_mut(e).remove::<B>();
    world.entity_mut(e).insert(Other);
    assert_eq!(world.get::<A>(e), Some(&A(10)));
    assert_eq!(world.get::<C>(e), Some(&C(30)));
}

#[test]
fn moving_one_entity_does_not_disturb_others_in_the_old_archetype() {
    let mut world = World::new();
    let ids: Vec<_> = (0..10)
        .map(|i| world.spawn((A(i), B(i * 2))).id())
        .collect();
    world.entity_mut(ids[3]).insert(C(0));
    world.entity_mut(ids[5]).remove::<B>();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(world.get::<A>(*id), Some(&A(i as i32)));
        if i == 5 {
            assert!(world.get::<B>(*id).is_none());
        } else {
            assert_eq!(world.get::<B>(*id), Some(&B(i as i32 * 2)));
        }
    }
}

#[test]
fn sparse_set_components_do_not_change_the_table_archetype_location() {
    let mut world = World::new();
    let e = world.spawn(A(1)).id();
    let table_before = world.entity(e).location().table_id;
    world.entity_mut(e).insert(Sparse(5));
    let table_after = world.entity(e).location().table_id;
    assert_eq!(table_before, table_after);
    assert_eq!(world.get::<Sparse>(e), Some(&Sparse(5)));
    world.entity_mut(e).remove::<Sparse>();
    assert!(world.get::<Sparse>(e).is_none());
    assert_eq!(world.get::<A>(e), Some(&A(1)));
}

#[test]
fn required_components_are_inserted_with_defaults() {
    let mut world = World::new();
    let e = world.spawn(Hero).id();
    assert_eq!(world.get::<Health>(e), Some(&Health(100)));
    assert!(world.entity(e).contains::<Tag>());
}

#[test]
fn explicit_values_override_required_component_defaults() {
    let mut world = World::new();
    let e = world.spawn((Hero, Health(5))).id();
    assert_eq!(world.get::<Health>(e), Some(&Health(5)));
}

#[test]
fn requirements_are_transitive() {
    let mut world = World::new();
    let e = world.spawn(Champion).id();
    assert!(world.entity(e).contains::<Hero>());
    assert_eq!(world.get::<Health>(e), Some(&Health(100)));
    assert!(world.entity(e).contains::<Tag>());
}

#[test]
fn removing_a_requiring_component_keeps_its_requirements() {
    let mut world = World::new();
    let e = world.spawn(Hero).id();
    world.entity_mut(e).remove::<Hero>();
    assert_eq!(world.get::<Health>(e), Some(&Health(100)));
}

#[test]
fn replacing_a_component_drops_the_old_value_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    let e = world.spawn(Droppy(Arc::clone(&drops))).id();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    world.entity_mut(e).insert(Droppy(Arc::clone(&drops)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn removing_a_component_drops_it_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    let e = world.spawn(Droppy(Arc::clone(&drops))).id();
    world.entity_mut(e).remove::<Droppy>();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    world.entity_mut(e).remove::<Droppy>();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn despawn_drops_all_components_of_the_entity() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    let e = world.spawn(Droppy(Arc::clone(&drops))).id();
    let keep = world.spawn(Droppy(Arc::clone(&drops))).id();
    world.despawn(e);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(world.get_entity(keep).is_ok());
}

#[test]
fn dropping_the_world_drops_remaining_components() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    for _ in 0..6 {
        world.spawn(Droppy(Arc::clone(&drops)));
    }
    drop(world);
    assert_eq!(drops.load(Ordering::SeqCst), 6);
}

#[test]
fn taking_a_component_transfers_ownership_without_dropping_it() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    let e = world.spawn(Droppy(Arc::clone(&drops))).id();
    let taken = world.entity_mut(e).take::<Droppy>().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(taken);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn insert_batch_applies_values_to_each_listed_entity() {
    let mut world = World::new();
    let ids: Vec<_> = (0..8).map(|_| world.spawn_empty().id()).collect();
    world.insert_batch(ids.iter().enumerate().map(|(i, e)| (*e, A(i as i32 * 3))));
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(world.get::<A>(*id), Some(&A(i as i32 * 3)));
    }
}

#[test]
fn try_insert_batch_reports_stale_entities_and_still_applies_live_ones() {
    let mut world = World::new();
    let live = world.spawn_empty().id();
    let dead = world.spawn_empty().id();
    world.despawn(dead);
    let result = world.try_insert_batch([(live, A(1)), (dead, A(2))]);
    assert!(result.is_err());
    assert_eq!(world.get::<A>(live), Some(&A(1)));
}

#[test]
fn inserting_into_a_stale_entity_via_get_entity_mut_is_rejected() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    world.despawn(e);
    assert!(world.get_entity_mut(e).is_err());
    assert_eq!(count::<A>(&mut world), 0);
}

#[test]
fn component_ids_are_stable_once_registered() {
    let mut world = World::new();
    assert!(world.component_id::<Sparse>().is_none());
    world.spawn(Sparse(1));
    let id = world.component_id::<Sparse>().unwrap();
    world.spawn(Sparse(2));
    assert_eq!(world.component_id::<Sparse>(), Some(id));
    assert_ne!(Some(id), world.component_id::<A>());
}

#[test]
fn register_component_creates_an_id_without_spawning() {
    let mut world = World::new();
    let id = world.register_component::<C>();
    assert_eq!(world.component_id::<C>(), Some(id));
    assert_eq!(count::<C>(&mut world), 0);
}

#[derive(Resource, Default)]
struct HookLog(Vec<String>);

#[derive(Component)]
#[component(on_add = hook_add, on_insert = hook_insert, on_discard = hook_discard, on_remove = hook_remove)]
struct Hooked(i32);

fn hook_add(
    mut world: engine_ecs::bevy_ecs::world::DeferredWorld,
    ctx: engine_ecs::bevy_ecs::lifecycle::HookContext,
) {
    let v = world.get::<Hooked>(ctx.entity).unwrap().0;
    world.resource_mut::<HookLog>().0.push(format!("add{v}"));
}
fn hook_insert(
    mut world: engine_ecs::bevy_ecs::world::DeferredWorld,
    ctx: engine_ecs::bevy_ecs::lifecycle::HookContext,
) {
    let v = world.get::<Hooked>(ctx.entity).unwrap().0;
    world.resource_mut::<HookLog>().0.push(format!("insert{v}"));
}
fn hook_discard(
    mut world: engine_ecs::bevy_ecs::world::DeferredWorld,
    ctx: engine_ecs::bevy_ecs::lifecycle::HookContext,
) {
    let v = world.get::<Hooked>(ctx.entity).unwrap().0;
    world
        .resource_mut::<HookLog>()
        .0
        .push(format!("discard{v}"));
}
fn hook_remove(
    mut world: engine_ecs::bevy_ecs::world::DeferredWorld,
    ctx: engine_ecs::bevy_ecs::lifecycle::HookContext,
) {
    let v = world.get::<Hooked>(ctx.entity).unwrap().0;
    world.resource_mut::<HookLog>().0.push(format!("remove{v}"));
}

#[test]
fn lifecycle_hooks_fire_in_documented_order_for_spawn_replace_and_remove() {
    let mut world = World::new();
    world.init_resource::<HookLog>();
    let e = world.spawn(Hooked(1)).id();
    world.entity_mut(e).insert(Hooked(2));
    world.entity_mut(e).remove::<Hooked>();
    assert_eq!(
        world.resource::<HookLog>().0,
        [
            "add1", "insert1", "discard1", "insert2", "discard2", "remove2"
        ]
    );
}

#[test]
fn despawn_runs_the_remove_hook_for_each_hooked_entity() {
    let mut world = World::new();
    world.init_resource::<HookLog>();
    let a = world.spawn(Hooked(10)).id();
    let b = world.spawn(Hooked(20)).id();
    world.despawn(a);
    world.despawn(b);
    let log = &world.resource::<HookLog>().0;
    assert_eq!(log.iter().filter(|s| s.starts_with("remove")).count(), 2);
    assert!(log.contains(&"remove10".to_string()));
    assert!(log.contains(&"remove20".to_string()));
}

/// Applies a seeded op stream to both a world and a shadow model and compares.
fn model_check(seed: u64, entities: usize, steps: usize) {
    let mut rng = Rng::new(seed);
    let mut world = World::new();
    let ids: Vec<Entity> = (0..entities).map(|_| world.spawn_empty().id()).collect();
    let mut model: Vec<(Option<i32>, Option<i32>, Option<i32>)> =
        vec![(None, None, None); entities];
    for _ in 0..steps {
        let i = rng.below(entities as u64) as usize;
        let v = rng.below(1000) as i32;
        match rng.below(6) {
            0 => {
                world.entity_mut(ids[i]).insert(A(v));
                model[i].0 = Some(v);
            }
            1 => {
                world.entity_mut(ids[i]).insert(B(v));
                model[i].1 = Some(v);
            }
            2 => {
                world.entity_mut(ids[i]).insert(C(v));
                model[i].2 = Some(v);
            }
            3 => {
                world.entity_mut(ids[i]).remove::<A>();
                model[i].0 = None;
            }
            4 => {
                world.entity_mut(ids[i]).remove::<B>();
                model[i].1 = None;
            }
            _ => {
                world.entity_mut(ids[i]).remove::<C>();
                model[i].2 = None;
            }
        }
    }
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(world.get::<A>(*id).map(|c| c.0), model[i].0, "A@{i}");
        assert_eq!(world.get::<B>(*id).map(|c| c.0), model[i].1, "B@{i}");
        assert_eq!(world.get::<C>(*id).map(|c| c.0), model[i].2, "C@{i}");
    }
    let expected_a = model.iter().filter(|m| m.0.is_some()).count();
    assert_eq!(count::<A>(&mut world), expected_a);
}

macro_rules! model_cases {
    ($($name:ident: $seed:expr, $entities:expr, $steps:expr;)*) => {
        $( #[test] fn $name() { model_check($seed, $entities, $steps); } )*
    };
}

model_cases! {
    model_matches_world_seed_1_small: 1, 3, 200;
    model_matches_world_seed_2_small: 2, 4, 400;
    model_matches_world_seed_17_medium: 17, 16, 1500;
    model_matches_world_seed_99_medium: 99, 24, 2500;
    model_matches_world_seed_4242_large: 4242, 64, 6000;
    model_matches_world_seed_90210_single_entity: 90210, 1, 800;
    model_matches_world_seed_31337_wide: 31337, 200, 4000;
}

#[test]
fn archetype_membership_matches_component_sets_after_random_edits() {
    let mut rng = Rng::new(5150);
    let mut world = World::new();
    let ids: Vec<Entity> = (0..32).map(|_| world.spawn_empty().id()).collect();
    for _ in 0..1500 {
        let id = ids[rng.below(32) as usize];
        match rng.below(4) {
            0 => {
                world.entity_mut(id).insert(A(1));
            }
            1 => {
                world.entity_mut(id).insert(B(1));
            }
            2 => {
                world.entity_mut(id).remove::<A>();
            }
            _ => {
                world.entity_mut(id).remove::<B>();
            }
        }
    }
    let mut by_archetype: HashMap<_, (bool, bool)> = HashMap::new();
    for id in &ids {
        let e = world.entity(*id);
        let key = (e.contains::<A>(), e.contains::<B>());
        let entry = by_archetype.entry(e.archetype().id()).or_insert(key);
        assert_eq!(*entry, key, "one archetype must map to one component set");
    }
}

#[test]
fn conservation_of_total_value_across_component_moves_between_entities() {
    let mut rng = Rng::new(808);
    let mut world = World::new();
    let ids: Vec<Entity> = (0..10).map(|_| world.spawn(Health(100)).id()).collect();
    for _ in 0..3000 {
        let from = ids[rng.below(10) as usize];
        let to = ids[rng.below(10) as usize];
        if from == to {
            continue;
        }
        let amount = rng.below(20) as i32;
        let available = world.get::<Health>(from).unwrap().0;
        let moved = amount.min(available);
        world.get_mut::<Health>(from).unwrap().0 -= moved;
        world.get_mut::<Health>(to).unwrap().0 += moved;
    }
    let total: i32 = ids.iter().map(|e| world.get::<Health>(*e).unwrap().0).sum();
    assert_eq!(total, 1000);
    assert!(ids.iter().all(|e| world.get::<Health>(*e).unwrap().0 >= 0));
}

macro_rules! spawn_scale_cases {
    ($($name:ident: $n:expr;)*) => {
        $( #[test] fn $name() {
            let mut world = World::new();
            let ids: Vec<_> = (0..$n).map(|i| world.spawn((A(i as i32), B(-(i as i32)))).id()).collect();
            let sum_a: i64 = world.query::<&A>().iter(&world).map(|a| i64::from(a.0)).sum();
            let sum_b: i64 = world.query::<&B>().iter(&world).map(|b| i64::from(b.0)).sum();
            assert_eq!(ids.len(), $n);
            assert_eq!(sum_a, -sum_b);
            assert_eq!(sum_a, ($n as i64) * ($n as i64 - 1) / 2);
        } )*
    };
}

spawn_scale_cases! {
    spawn_scale_one: 1;
    spawn_scale_seven: 7;
    spawn_scale_sixty_four: 64;
    spawn_scale_one_thousand: 1000;
    spawn_scale_five_thousand_and_three: 5003;
}
