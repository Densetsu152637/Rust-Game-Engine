//! Deferred command semantics and visibility between systems and phases.
mod common;

use common::*;
use engine_core::bevy_ecs::schedule::{ApplyDeferred, ScheduleBuildSettings};
use engine_core::prelude::*;

fn live(q: Query<&Health>, mut seen: ResMut<Seen>) {
    seen.0.push(q.iter().count() as i64);
}

fn spawn_one(mut commands: Commands) {
    commands.spawn(Health(1));
}

fn app_with_seen() -> EngineApp {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app
}

fn seen(app: &EngineApp) -> Vec<i64> {
    app.world().resource::<Seen>().0.clone()
}

#[test]
fn commands_are_invisible_to_the_issuing_system() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(
        |mut commands: Commands, q: Query<&Health>, mut seen: ResMut<Seen>| {
            commands.spawn(Health(1));
            seen.0.push(q.iter().count() as i64);
        },
    );
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [0]);
    assert_eq!(count::<Health>(app.world_mut()), 1);
}

#[test]
fn chained_systems_see_earlier_commands() {
    let mut app = app_with_seen();
    update(&mut app).add_systems((spawn_one, live).chain());
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1, 2]);
}

#[test]
fn explicit_before_after_edges_also_publish_commands() {
    let mut app = app_with_seen();
    update(&mut app).add_systems((live.after(spawn_one), spawn_one));
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn set_ordering_publishes_commands_to_later_sets() {
    #[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
    enum S {
        Spawn,
        Read,
    }
    let mut app = app_with_seen();
    update(&mut app)
        .configure_sets((S::Spawn, S::Read).chain())
        .add_systems((spawn_one.in_set(S::Spawn), live.in_set(S::Read)));
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn reverse_order_defers_commands_to_the_next_tick() {
    let mut app = app_with_seen();
    update(&mut app).add_systems((live, spawn_one).chain());
    app.run_fixed(3, DT).unwrap();
    assert_eq!(seen(&app), [0, 1, 2]);
}

#[test]
fn all_commands_are_applied_by_the_end_of_the_stage() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(spawn_one);
    app.tick(DT).unwrap();
    assert_eq!(count::<Health>(app.world_mut()), 1);
}

#[test]
fn update_commands_are_visible_to_the_extract_stage_of_the_same_tick() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(spawn_one);
    app.schedule_mut(Stage::Extract).add_systems(live);
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn startup_commands_are_visible_to_the_first_update() {
    let mut app = app_with_seen();
    app.schedule_mut(Stage::Startup).add_systems(spawn_one);
    update(&mut app).add_systems(live);
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn shutdown_commands_are_applied_before_the_world_is_returned() {
    let mut app = app_with_seen();
    app.schedule_mut(Stage::Shutdown)
        .add_systems(|mut c: Commands| c.insert_resource(Gold(99)));
    app.run_fixed(1, DT).unwrap();
    assert_eq!(app.world().resource::<Gold>().0, 99);
}

#[derive(Resource, Debug)]
struct Gold(i64);

#[test]
fn render_stage_commands_apply_to_the_render_world_only() {
    let mut app = app_with_seen();
    app.schedule_mut(Stage::Render)
        .add_systems(|mut c: Commands| {
            c.spawn(Health(5));
        });
    app.tick(DT).unwrap();
    assert_eq!(count::<Health>(app.world_mut()), 0);
    assert_eq!(count::<Health>(app.render_world_mut()), 1);
}

#[test]
fn queue_order_within_one_system_is_preserved() {
    let mut app = app_with_seen();
    let e = app.world_mut().spawn(Health(0)).id();
    app.world_mut().insert_resource(Target(e));
    update(&mut app).add_systems(|t: Res<Target>, mut c: Commands| {
        c.entity(t.0).insert(Health(1));
        c.entity(t.0).insert(Health(2));
        c.entity(t.0).insert(Health(3));
    });
    app.tick(DT).unwrap();
    assert_eq!(app.world().get::<Health>(e), Some(&Health(3)));
}

#[derive(Resource)]
struct Target(Entity);

#[test]
fn insert_then_remove_in_one_queue_ends_absent() {
    let mut app = app_with_seen();
    let e = app.world_mut().spawn_empty().id();
    app.world_mut().insert_resource(Target(e));
    update(&mut app).add_systems(|t: Res<Target>, mut c: Commands| {
        c.entity(t.0).insert(Tag);
        c.entity(t.0).remove::<Tag>();
    });
    app.tick(DT).unwrap();
    assert!(app.world().get::<Tag>(e).is_none());
}

#[test]
fn remove_then_insert_in_one_queue_ends_present() {
    let mut app = app_with_seen();
    let e = app.world_mut().spawn(Tag).id();
    app.world_mut().insert_resource(Target(e));
    update(&mut app).add_systems(|t: Res<Target>, mut c: Commands| {
        c.entity(t.0).remove::<Tag>();
        c.entity(t.0).insert(Tag);
    });
    app.tick(DT).unwrap();
    assert!(app.world().get::<Tag>(e).is_some());
}

#[test]
fn spawn_then_despawn_in_one_queue_leaves_nothing() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands| {
        let id = c.spawn(Health(1)).id();
        c.entity(id).despawn();
    });
    app.tick(DT).unwrap();
    assert_eq!(count::<Health>(app.world_mut()), 0);
}

#[test]
fn spawn_batch_applies_every_bundle() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands| {
        c.spawn_batch((0..100).map(Health));
    });
    app.tick(DT).unwrap();
    let total: i32 = app
        .world_mut()
        .query::<&Health>()
        .iter(app.world())
        .map(|h| h.0)
        .sum();
    assert_eq!(total, 4950);
}

#[test]
fn ten_thousand_deferred_spawns_are_all_applied() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands| {
        for i in 0..10_000 {
            c.spawn(Health(i));
        }
    });
    app.tick(DT).unwrap();
    assert_eq!(count::<Health>(app.world_mut()), 10_000);
}

#[test]
fn queue_is_drained_after_application_and_not_replayed() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands, time: Res<Time>| {
        if time.tick == 1 {
            c.spawn(Health(1));
        }
    });
    app.run_fixed(5, DT).unwrap();
    assert_eq!(count::<Health>(app.world_mut()), 1);
}

#[test]
fn insert_if_new_does_not_overwrite_when_applied() {
    let mut app = app_with_seen();
    let e = app.world_mut().spawn(Health(1)).id();
    app.world_mut().insert_resource(Target(e));
    update(&mut app).add_systems(|t: Res<Target>, mut c: Commands| {
        c.entity(t.0).insert_if_new((Health(50), Tag));
    });
    app.tick(DT).unwrap();
    assert_eq!(app.world().get::<Health>(e), Some(&Health(1)));
    assert!(app.world().get::<Tag>(e).is_some());
}

#[test]
fn conditional_insert_obeys_the_condition_at_queue_time() {
    let mut app = app_with_seen();
    let a = app.world_mut().spawn_empty().id();
    let b = app.world_mut().spawn_empty().id();
    app.world_mut().insert_resource(Pair(a, b));
    update(&mut app).add_systems(|p: Res<Pair>, mut c: Commands| {
        c.entity(p.0).insert_if(Tag, || true);
        c.entity(p.1).insert_if(Tag, || false);
    });
    app.tick(DT).unwrap();
    assert!(app.world().get::<Tag>(a).is_some());
    assert!(app.world().get::<Tag>(b).is_none());
}

#[derive(Resource)]
struct Pair(Entity, Entity);

#[test]
fn custom_closure_commands_run_with_exclusive_world_access() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands| {
        c.queue(|world: &mut World| {
            world.insert_resource(Gold(7));
            let e = world.spawn(Health(1)).id();
            world.entity_mut(e).insert(Tag);
        });
    });
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Gold>().0, 7);
    assert_eq!(count::<Tag>(app.world_mut()), 1);
}

#[test]
fn exclusive_systems_see_commands_from_earlier_systems() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(
        (spawn_one, |world: &mut World| {
            let n = world.query::<&Health>().iter(world).count() as i64;
            world.resource_mut::<Seen>().0.push(n);
        })
            .chain(),
    );
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1]);
}

#[test]
fn explicit_apply_deferred_is_a_sync_point_without_auto_insertion() {
    let mut world = World::new();
    world.init_resource::<Seen>();
    let mut schedule = Schedule::default();
    schedule.set_build_settings(ScheduleBuildSettings {
        auto_insert_apply_deferred: false,
        ..Default::default()
    });
    schedule.add_systems((spawn_one, ApplyDeferred, live).chain());
    schedule.run(&mut world);
    assert_eq!(world.resource::<Seen>().0, [1]);
}

#[test]
fn without_auto_insertion_chained_readers_miss_commands_until_end_of_schedule() {
    let mut world = World::new();
    world.init_resource::<Seen>();
    let mut schedule = Schedule::default();
    schedule.set_build_settings(ScheduleBuildSettings {
        auto_insert_apply_deferred: false,
        ..Default::default()
    });
    schedule.add_systems((spawn_one, live).chain());
    schedule.run(&mut world);
    assert_eq!(world.resource::<Seen>().0, [0]);
    assert_eq!(
        count::<Health>(&mut world),
        1,
        "end-of-schedule flush still applies"
    );
}

#[test]
fn disabling_the_final_flush_leaves_commands_pending() {
    let mut world = World::new();
    let mut schedule = Schedule::default();
    schedule.set_apply_final_deferred(false);
    schedule.add_systems(spawn_one);
    schedule.run(&mut world);
    assert_eq!(count::<Health>(&mut world), 0);
    schedule.apply_deferred(&mut world);
    assert_eq!(count::<Health>(&mut world), 1);
}

#[test]
fn world_commands_are_applied_only_on_flush() {
    let mut world = World::new();
    world.commands().spawn(Health(3));
    assert_eq!(count::<Health>(&mut world), 0);
    world.flush();
    assert_eq!(count::<Health>(&mut world), 1);
}

#[test]
#[should_panic]
fn infallible_insert_on_a_stale_entity_panics_when_applied() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    world.despawn(e);
    world.commands().entity(e).insert(Tag);
    world.flush();
}

#[test]
fn try_insert_on_a_stale_entity_is_silently_ignored_when_applied() {
    let mut world = World::new();
    let e = world.spawn_empty().id();
    world.despawn(e);
    world.commands().entity(e).try_insert(Tag);
    world.flush();
    assert_eq!(count::<Tag>(&mut world), 0);
}

#[test]
fn reserved_entity_ids_from_parallel_systems_never_collide() {
    let mut app = app_with_seen();
    update(&mut app).add_systems((
        |mut c: Commands| {
            for _ in 0..200 {
                c.spawn(Tag);
            }
        },
        |mut c: Commands| {
            for _ in 0..200 {
                c.spawn(Other);
            }
        },
    ));
    app.tick(DT).unwrap();
    let mut all: Vec<Entity> = app
        .world_mut()
        .query_filtered::<Entity, Or<(With<Tag>, With<Other>)>>()
        .iter(app.world())
        .collect();
    assert_eq!(all.len(), 400);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 400);
}

#[test]
fn command_application_order_of_unordered_systems_is_stable_across_runs() {
    fn run() -> Vec<i64> {
        let mut app = app_with_seen();
        update(&mut app).add_systems((
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(1)),
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(2)),
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(3)),
        ));
        app.tick(DT).unwrap();
        seen(&app)
    }
    let first = run();
    assert_eq!(first.len(), 3);
    for _ in 0..40 {
        assert_eq!(run(), first);
    }
}

#[test]
fn chained_command_queues_apply_in_chain_order() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(
        (
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(1)),
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(2)),
            |mut c: Commands| c.queue(|w: &mut World| w.resource_mut::<Seen>().0.push(3)),
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    assert_eq!(seen(&app), [1, 2, 3]);
}

#[test]
fn one_shot_registered_systems_apply_their_commands_when_run() {
    let mut world = World::new();
    let id = world.register_system(|mut c: Commands| {
        c.spawn(Health(1));
    });
    world.run_system(id).unwrap();
    world.run_system(id).unwrap();
    assert_eq!(count::<Health>(&mut world), 2);
}

#[test]
fn one_shot_system_can_return_values_and_carry_state() {
    let mut world = World::new();
    let id = world.register_system(|mut n: Local<i32>| {
        *n += 1;
        *n
    });
    assert_eq!(world.run_system(id).unwrap(), 1);
    assert_eq!(world.run_system(id).unwrap(), 2);
    assert_eq!(world.run_system(id).unwrap(), 3);
}

#[test]
fn observers_fire_when_a_component_is_added_via_commands() {
    let mut app = app_with_seen();
    app.world_mut()
        .add_observer(|_: On<Add<Tag>>, mut seen: ResMut<Seen>| {
            seen.0.push(100);
        });
    update(&mut app).add_systems(|mut c: Commands, time: Res<Time>| {
        if time.tick <= 2 {
            c.spawn(Tag);
        }
    });
    app.run_fixed(4, DT).unwrap();
    assert_eq!(seen(&app), [100, 100]);
}

#[test]
fn observers_see_the_entity_that_received_the_component() {
    let mut app = app_with_seen();
    app.world_mut()
        .add_observer(|on: On<Add<Tag>>, mut commands: Commands| {
            commands.entity(on.event_target()).insert(Frozen);
        });
    update(&mut app).add_systems(|mut c: Commands, time: Res<Time>| {
        if time.tick == 1 {
            c.spawn(Tag);
            c.spawn(Other);
        }
    });
    app.tick(DT).unwrap();
    assert_eq!(count_with::<(With<Tag>, With<Frozen>)>(app.world_mut()), 1);
    assert_eq!(
        count_with::<(With<Other>, With<Frozen>)>(app.world_mut()),
        0
    );
}

#[test]
fn entity_commands_id_is_stable_and_usable_for_follow_up_commands() {
    let mut app = app_with_seen();
    update(&mut app).add_systems(|mut c: Commands, time: Res<Time>| {
        if time.tick == 1 {
            let parent = c.spawn(Health(10)).id();
            let child = c.spawn(Health(1)).id();
            c.entity(child).insert(Linked(parent));
            c.entity(parent).insert(Tag);
        }
    });
    app.tick(DT).unwrap();
    let (child_health, parent) = {
        let mut q = app.world_mut().query::<(&Health, &Linked)>();
        let (h, l) = q.iter(app.world()).next().unwrap();
        (h.0, l.0)
    };
    assert_eq!(child_health, 1);
    assert_eq!(app.world().get::<Health>(parent), Some(&Health(10)));
    assert!(app.world().get::<Tag>(parent).is_some());
}

#[derive(Component)]
struct Linked(Entity);
