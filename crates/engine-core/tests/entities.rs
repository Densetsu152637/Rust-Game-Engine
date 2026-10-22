//! Entity identity, generations and stale-handle behaviour.
mod common;

use std::collections::{HashMap, HashSet};

use common::*;
use engine_core::bevy_ecs::entity::{EntityGeneration, EntityIndex};
use engine_core::prelude::*;

fn spawn_value(world: &mut World, value: i32) -> Entity {
    world.spawn(Health(value)).id()
}

#[derive(Resource)]
struct Handle(Entity);

#[test]
fn spawned_entities_are_distinct_handles() {
    let mut world = World::new();
    let ids: HashSet<Entity> = (0..256).map(|i| spawn_value(&mut world, i)).collect();
    assert_eq!(ids.len(), 256);
}

#[test]
fn spawn_empty_creates_live_entity_without_components() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    assert!(world.get_entity(e).is_ok());
    assert!(world.get::<Health>(e).is_none());
    assert!(world.entities().contains_spawned(e));
}

#[test]
fn entity_count_tracks_spawn_and_despawn_relative_to_baseline() {
    let mut world = World::new();
    let baseline = world.entity_count();
    let ids: Vec<_> = (0..40).map(|i| spawn_value(&mut world, i)).collect();
    assert_eq!(world.entity_count(), baseline + 40);
    for id in ids.iter().take(15) {
        assert!(world.despawn(*id));
    }
    assert_eq!(world.entity_count(), baseline + 25);
    for id in ids.iter().skip(15) {
        assert!(world.despawn(*id));
    }
    assert_eq!(world.entity_count(), baseline);
}

#[test]
fn despawn_reports_true_once_then_false() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 1);
    assert!(world.despawn(e));
    assert!(!world.despawn(e));
    assert!(!world.despawn(e));
}

#[test]
fn try_despawn_of_stale_handle_returns_error_without_side_effects() {
    let mut world = World::new();
    let stale = spawn_value(&mut world, 1);
    let live = spawn_value(&mut world, 2);
    world.despawn(stale);
    assert!(world.try_despawn(stale).is_err());
    assert_eq!(world.get::<Health>(live), Some(&Health(2)));
}

#[test]
fn stale_handle_reads_none_after_despawn() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 5);
    assert_eq!(world.get::<Health>(e), Some(&Health(5)));
    world.despawn(e);
    assert!(world.get::<Health>(e).is_none());
    assert!(world.get_entity(e).is_err());
    assert!(!world.entities().contains_spawned(e));
}

#[test]
fn get_entity_mut_on_stale_handle_is_an_error() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 5);
    world.despawn(e);
    assert!(world.get_entity_mut(e).is_err());
}

#[test]
#[should_panic]
fn entity_mut_on_stale_handle_panics() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 5);
    world.despawn(e);
    let _ = world.entity_mut(e);
}

#[test]
#[should_panic]
fn entity_on_stale_handle_panics() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 5);
    world.despawn(e);
    let _ = world.entity(e);
}

#[test]
fn despawning_one_entity_leaves_neighbours_untouched() {
    let mut world = World::new();
    let ids: Vec<_> = (0..10).map(|i| spawn_value(&mut world, i * 10)).collect();
    world.despawn(ids[4]);
    for (i, id) in ids.iter().enumerate() {
        if i == 4 {
            assert!(world.get::<Health>(*id).is_none());
        } else {
            assert_eq!(world.get::<Health>(*id), Some(&Health(i as i32 * 10)));
        }
    }
}

#[test]
fn stale_handles_never_alias_new_entities_under_seeded_churn() {
    let mut rng = Rng::new(0xC0FFEE);
    let mut world = World::new();
    let mut live: HashMap<Entity, i32> = HashMap::new();
    let mut dead: HashSet<Entity> = HashSet::new();
    for step in 0..2_000 {
        if live.is_empty() || rng.chance(3, 5) {
            let e = spawn_value(&mut world, step);
            assert!(live.insert(e, step).is_none(), "handle reused while live");
            assert!(!dead.contains(&e), "dead handle {e:?} resurrected");
        } else {
            let keys: Vec<_> = live.keys().copied().collect();
            let victim = keys[rng.below(keys.len() as u64) as usize];
            assert!(world.despawn(victim));
            live.remove(&victim);
            dead.insert(victim);
        }
    }
    for (e, v) in &live {
        assert_eq!(world.get::<Health>(*e), Some(&Health(*v)));
    }
    for e in &dead {
        assert!(
            world.get::<Health>(*e).is_none(),
            "{e:?} aliased a live slot"
        );
    }
}

#[test]
fn reused_index_always_carries_a_newer_generation() {
    let mut world = World::new();
    let mut seen_index: HashMap<EntityIndex, Entity> = HashMap::new();
    let mut reuses = 0;
    for round in 0..200 {
        let batch: Vec<_> = (0..8)
            .map(|i| spawn_value(&mut world, round * 8 + i))
            .collect();
        for e in &batch {
            if let Some(previous) = seen_index.insert(e.index(), *e) {
                reuses += 1;
                assert_ne!(previous, *e);
                assert_ne!(previous.generation(), e.generation());
                assert_eq!(
                    previous.generation().cmp_approx(&e.generation()),
                    std::cmp::Ordering::Less
                );
            }
        }
        for e in batch {
            world.despawn(e);
        }
    }
    assert!(reuses > 0, "freed indices must eventually be recycled");
}

#[test]
fn live_indices_are_unique_at_any_time() {
    let mut rng = Rng::new(77);
    let mut world = World::new();
    let mut live = Vec::new();
    for i in 0..500 {
        live.push(spawn_value(&mut world, i));
        if rng.chance(1, 2) {
            let idx = rng.below(live.len() as u64) as usize;
            world.despawn(live.swap_remove(idx));
        }
        let indices: HashSet<_> = live.iter().map(|e| e.index()).collect();
        assert_eq!(indices.len(), live.len());
    }
}

#[test]
fn forged_handle_with_wrong_generation_does_not_resolve() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 1);
    let forged = Entity::from_index_and_generation(e.index(), e.generation().after_versions(1));
    assert_ne!(forged, e);
    assert!(world.get::<Health>(forged).is_none());
    assert!(world.get_entity(forged).is_err());
    assert_eq!(world.get::<Health>(e), Some(&Health(1)));
}

#[test]
fn handle_with_same_index_and_generation_equals_original() {
    let mut world = World::new();
    let e = spawn_value(&mut world, 1);
    let rebuilt = Entity::from_index_and_generation(e.index(), e.generation());
    assert_eq!(rebuilt, e);
    assert_eq!(world.get::<Health>(rebuilt), Some(&Health(1)));
}

#[test]
fn entity_bits_round_trip_for_many_entities() {
    let mut world = World::new();
    for i in 0..64 {
        let e = spawn_value(&mut world, i);
        assert_eq!(Entity::from_bits(e.to_bits()), e);
        assert_eq!(Entity::try_from_bits(e.to_bits()), Some(e));
    }
}

#[test]
fn bits_with_reserved_max_index_are_rejected() {
    assert!(Entity::try_from_bits(0).is_none() && Entity::try_from_bits(7u64 << 32).is_none());
}

#[test]
#[should_panic]
fn from_bits_with_reserved_max_index_panics() {
    let _ = Entity::from_bits(0);
}

#[test]
fn from_raw_u32_rejects_only_the_reserved_index() {
    assert!(Entity::from_raw_u32(u32::MAX).is_none());
    assert_eq!(Entity::from_raw_u32(0).unwrap().index_u32(), 0);
    assert_eq!(Entity::from_raw_u32(12_345).unwrap().index_u32(), 12_345);
}

#[test]
fn from_index_always_uses_first_generation() {
    let e = Entity::from_raw_u32(9).unwrap();
    assert_eq!(e.generation(), EntityGeneration::FIRST);
    assert_eq!(e.index_u32(), 9);
}

#[test]
fn generation_arithmetic_wraps_and_reports_aliasing() {
    let first = EntityGeneration::FIRST;
    assert_eq!(first.after_versions(3).to_bits(), 3);
    let (wrapped, aliased) = first.after_versions_and_could_alias(u32::MAX);
    assert!(!aliased);
    assert_eq!(wrapped.to_bits(), u32::MAX);
    let (again, aliased) = wrapped.after_versions_and_could_alias(2);
    assert!(aliased);
    assert_eq!(again.to_bits(), 1);
}

#[test]
fn generation_comparison_orders_nearby_generations() {
    let g0 = EntityGeneration::FIRST;
    let g1 = g0.after_versions(1);
    assert_eq!(g0.cmp_approx(&g1), std::cmp::Ordering::Less);
    assert_eq!(g1.cmp_approx(&g0), std::cmp::Ordering::Greater);
    assert_eq!(g1.cmp_approx(&g1), std::cmp::Ordering::Equal);
}

#[test]
fn placeholder_is_never_a_live_entity_in_a_fresh_world() {
    let mut world = World::new();
    for i in 0..10 {
        spawn_value(&mut world, i);
    }
    assert!(world.get_entity(Entity::PLACEHOLDER).is_err());
    assert!(!world.entities().contains_spawned(Entity::PLACEHOLDER));
}

#[test]
fn entities_can_key_hash_maps() {
    let mut world = World::new();
    let a = spawn_value(&mut world, 1);
    let b = spawn_value(&mut world, 2);
    let mut map = HashMap::new();
    map.insert(a, "a");
    map.insert(b, "b");
    assert_eq!(map[&a], "a");
    assert_eq!(map[&b], "b");
    assert_ne!(a, b);
}

#[test]
fn entities_have_a_total_order_consistent_with_equality() {
    let mut world = World::new();
    let mut ids: Vec<_> = (0..20).map(|i| spawn_value(&mut world, i)).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    ids.sort_by_key(|e| e.to_bits());
    assert_eq!(sorted, ids);
    sorted.dedup();
    assert_eq!(sorted.len(), 20);
}

#[test]
fn iter_entities_lists_every_spawned_entity() {
    let mut world = World::new();
    let spawned: HashSet<_> = (0..12).map(|i| spawn_value(&mut world, i)).collect();
    let listed: HashSet<_> = world.iter_entities().map(|e| e.id()).collect();
    assert!(spawned.is_subset(&listed));
}

#[test]
fn spawn_batch_creates_every_requested_entity_with_its_value() {
    let mut world = World::new();
    let ids: Vec<_> = world.spawn_batch((0..25).map(Health)).collect();
    assert_eq!(ids.len(), 25);
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(world.get::<Health>(*id), Some(&Health(i as i32)));
    }
}

#[test]
fn despawn_all_removes_only_matching_entities() {
    let mut world = World::new();
    let keep = world.spawn((Health(1), Other)).id();
    for _ in 0..5 {
        world.spawn((Health(2), Tag));
    }
    world.despawn_all::<With<Tag>>();
    assert_eq!(count::<Tag>(&mut world), 0);
    assert!(world.get_entity(keep).is_ok());
}

#[test]
fn clear_entities_invalidates_existing_handles() {
    let mut world = World::new();
    let ids: Vec<_> = (0..10).map(|i| spawn_value(&mut world, i)).collect();
    world.clear_entities();
    for id in ids {
        assert!(world.get::<Health>(id).is_none());
    }
    assert_eq!(count::<Health>(&mut world), 0);
}

#[test]
fn commands_spawn_returns_a_usable_handle_before_application() {
    let mut app = app();
    update(&mut app).add_systems(|mut commands: Commands, time: Res<Time>| {
        if time.tick == 1 {
            let id = commands.spawn(Health(3)).id();
            commands.entity(id).insert(Tag);
            commands.insert_resource(Handle(id));
        }
    });
    app.tick(DT).unwrap();
    let id = app.world().resource::<Handle>().0;
    assert_eq!(app.world().get::<Health>(id), Some(&Health(3)));
    assert!(app.world().get::<Tag>(id).is_some());
}

#[test]
fn entity_links_between_components_go_stale_after_target_despawn() {
    #[derive(Component)]
    struct Target(Entity);
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let target = app.world_mut().spawn(Health(1)).id();
    let follower = app.world_mut().spawn(Target(target)).id();
    update(&mut app).add_systems(
        |links: Query<&Target>, health: Query<&Health>, mut seen: ResMut<Seen>| {
            for link in &links {
                seen.0.push(i64::from(health.get(link.0).is_ok()));
            }
        },
    );
    app.tick(DT).unwrap();
    app.world_mut().despawn(target);
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0]);
    assert!(app.world().get::<Target>(follower).is_some());
}

#[test]
fn commands_despawn_is_deferred_until_command_application() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(Health(1)).id();
    app.world_mut().insert_resource(Handle(e));
    update(&mut app).add_systems(
        (
            |handle: Res<Handle>,
             mut commands: Commands,
             health: Query<&Health>,
             mut seen: ResMut<Seen>| {
                commands.entity(handle.0).despawn();
                seen.0.push(i64::from(health.get(handle.0).is_ok()));
            },
            |handle: Res<Handle>, health: Query<&Health>, mut seen: ResMut<Seen>| {
                seen.0.push(i64::from(health.get(handle.0).is_ok()));
            },
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0]);
}

#[test]
fn try_despawn_via_commands_twice_does_not_panic() {
    let mut app = app();
    let e = app.world_mut().spawn(Health(1)).id();
    app.world_mut().insert_resource(Handle(e));
    update(&mut app).add_systems(|handle: Res<Handle>, mut commands: Commands| {
        commands.entity(handle.0).try_despawn();
        commands.entity(handle.0).try_despawn();
    });
    app.tick(DT).unwrap();
    assert!(app.world().get_entity(e).is_err());
}

#[test]
fn try_insert_on_despawned_entity_does_not_resurrect_it() {
    let mut app = app();
    let e = app.world_mut().spawn(Health(1)).id();
    app.world_mut().despawn(e);
    app.world_mut().insert_resource(Handle(e));
    update(&mut app).add_systems(|handle: Res<Handle>, mut commands: Commands| {
        commands.entity(handle.0).try_insert(Tag);
    });
    app.tick(DT).unwrap();
    assert!(app.world().get_entity(e).is_err());
    assert_eq!(count::<Tag>(app.world_mut()), 0);
}

#[test]
fn entity_handles_survive_unrelated_structural_changes() {
    let mut world = World::new();
    let e = world.spawn(Health(7)).id();
    for i in 0..50 {
        let other = world.spawn((Health(i), Tag)).id();
        world.entity_mut(other).insert(Other);
        world.entity_mut(other).remove::<Tag>();
        if i % 2 == 0 {
            world.despawn(other);
        }
    }
    assert_eq!(world.get::<Health>(e), Some(&Health(7)));
}

#[test]
fn archetype_moves_do_not_change_entity_identity() {
    let mut world = World::new();
    let e = world.spawn(Health(7)).id();
    world.entity_mut(e).insert((Tag, Other, Frozen));
    world.entity_mut(e).remove::<Other>();
    world.entity_mut(e).insert(Pos { x: 1, y: 2 });
    assert_eq!(world.entity(e).id(), e);
    assert_eq!(world.get::<Health>(e), Some(&Health(7)));
}

#[test]
fn engine_app_world_spawns_survive_ticks() {
    let mut app = app();
    let ids: Vec<_> = (0..10)
        .map(|i| app.world_mut().spawn(Health(i)).id())
        .collect();
    app.run_fixed(5, DT).unwrap();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(app.world().get::<Health>(*id), Some(&Health(i as i32)));
    }
}
