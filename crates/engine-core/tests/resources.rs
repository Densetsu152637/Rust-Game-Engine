//! Resource storage, change detection, conditions and ownership.
mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use common::*;
use engine_core::bevy_ecs::schedule::common_conditions::{
    resource_added, resource_changed, resource_equals, resource_exists, resource_removed,
};
use engine_core::prelude::*;

#[derive(Resource, Debug, Clone, PartialEq, Default)]
struct Gold(i64);

#[derive(Resource, Debug, Clone, PartialEq, Default)]
struct Bank(i64);

#[derive(Resource, Debug, PartialEq)]
struct Config {
    gravity: i32,
    name: String,
}

impl FromWorld for Config {
    fn from_world(world: &mut World) -> Self {
        let base = world.get_resource::<Gold>().map_or(1, |g| g.0 as i32);
        Config {
            gravity: base * 10,
            name: "from-world".into(),
        }
    }
}

#[derive(Resource)]
struct Generic<T: Send + Sync + 'static>(T);

#[derive(Resource)]
struct Droppy(Arc<AtomicUsize>);
impl Drop for Droppy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn insert_then_read_returns_the_value() {
    let mut world = World::new();
    world.insert_resource(Gold(5));
    assert_eq!(world.resource::<Gold>(), &Gold(5));
}

#[test]
fn get_resource_is_none_before_insertion() {
    let world = World::new();
    assert!(world.get_resource::<Gold>().is_none());
    assert!(!world.contains_resource::<Gold>());
}

#[test]
#[should_panic]
fn reading_a_missing_resource_panics() {
    let world = World::new();
    let _ = world.resource::<Gold>();
}

#[test]
fn insert_replaces_an_existing_resource() {
    let mut world = World::new();
    world.insert_resource(Gold(1));
    world.insert_resource(Gold(2));
    assert_eq!(world.resource::<Gold>(), &Gold(2));
}

#[test]
fn init_resource_uses_default_and_does_not_overwrite() {
    let mut world = World::new();
    world.init_resource::<Gold>();
    assert_eq!(world.resource::<Gold>(), &Gold(0));
    world.resource_mut::<Gold>().0 = 9;
    world.init_resource::<Gold>();
    assert_eq!(world.resource::<Gold>(), &Gold(9));
}

#[test]
fn init_resource_runs_from_world_with_access_to_other_resources() {
    let mut world = World::new();
    world.insert_resource(Gold(4));
    world.init_resource::<Config>();
    let config = world.resource::<Config>();
    assert_eq!((config.gravity, config.name.as_str()), (40, "from-world"));
}

#[test]
fn remove_resource_returns_the_value_once() {
    let mut world = World::new();
    world.insert_resource(Gold(7));
    assert_eq!(world.remove_resource::<Gold>(), Some(Gold(7)));
    assert_eq!(world.remove_resource::<Gold>(), None);
    assert!(!world.contains_resource::<Gold>());
}

#[test]
fn resources_of_different_types_are_independent() {
    let mut world = World::new();
    world.insert_resource(Gold(1));
    world.insert_resource(Bank(2));
    world.remove_resource::<Gold>();
    assert_eq!(world.resource::<Bank>(), &Bank(2));
}

#[test]
fn generic_resources_with_different_parameters_are_independent() {
    let mut world = World::new();
    world.insert_resource(Generic(1_u8));
    world.insert_resource(Generic("text"));
    assert_eq!(world.resource::<Generic<u8>>().0, 1);
    assert_eq!(world.resource::<Generic<&'static str>>().0, "text");
    assert!(!world.contains_resource::<Generic<u16>>());
}

#[test]
fn resource_mut_mutations_persist() {
    let mut world = World::new();
    world.insert_resource(Gold(1));
    world.resource_mut::<Gold>().0 += 99;
    assert_eq!(world.resource::<Gold>().0, 100);
}

#[test]
fn get_resource_mut_is_none_when_absent_and_some_when_present() {
    let mut world = World::new();
    assert!(world.get_resource_mut::<Gold>().is_none());
    world.insert_resource(Gold(1));
    world.get_resource_mut::<Gold>().unwrap().0 = 3;
    assert_eq!(world.resource::<Gold>().0, 3);
}

#[test]
fn replacing_a_resource_drops_the_old_value() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    world.insert_resource(Droppy(Arc::clone(&drops)));
    world.insert_resource(Droppy(Arc::clone(&drops)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn removing_a_resource_transfers_ownership_without_dropping() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    world.insert_resource(Droppy(Arc::clone(&drops)));
    let taken = world.remove_resource::<Droppy>().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(taken);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn dropping_the_world_drops_resources_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut world = World::new();
    world.insert_resource(Droppy(Arc::clone(&drops)));
    drop(world);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn world_level_resource_change_flags_follow_mutation() {
    let mut world = World::new();
    world.insert_resource(Gold(1));
    assert!(world.is_resource_added::<Gold>());
    assert!(world.is_resource_changed::<Gold>());
    assert!(world.get_resource_change_ticks::<Gold>().is_some());
    assert!(world.get_resource_change_ticks::<Bank>().is_none());
}

#[test]
fn resource_scope_gives_world_access_and_restores_the_resource() {
    let mut world = World::new();
    world.insert_resource(Gold(10));
    let spawned = world.resource_scope(|world: &mut World, mut gold: Mut<Gold>| {
        gold.0 += 1;
        world.spawn(Health(gold.0 as i32)).id()
    });
    assert_eq!(world.resource::<Gold>().0, 11);
    assert_eq!(world.get::<Health>(spawned), Some(&Health(11)));
}

#[test]
fn try_resource_scope_is_none_for_missing_resources() {
    let mut world = World::new();
    assert!(
        world
            .try_resource_scope(|_: &mut World, _: Mut<Gold>| 1)
            .is_none()
    );
    world.insert_resource(Gold(0));
    assert_eq!(
        world.try_resource_scope(|_: &mut World, _: Mut<Gold>| 1),
        Some(1)
    );
}

#[test]
fn resource_entity_stops_carrying_the_value_after_removal() {
    let mut world = World::new();
    assert!(world.resource_entity::<Gold>().is_none());
    world.insert_resource(Gold(1));
    let entity = world
        .resource_entity::<Gold>()
        .expect("resource has an entity");
    assert!(world.get_entity(entity).is_ok());
    assert_eq!(world.get::<Gold>(entity), Some(&Gold(1)));
    world.remove_resource::<Gold>();
    assert!(world.get::<Gold>(entity).is_none());
    assert!(!world.contains_resource::<Gold>());
}

#[test]
fn non_send_resources_hold_thread_local_types() {
    let mut world = World::new();
    let shared = Rc::new(RefCell::new(vec![1, 2]));
    world.insert_non_send(Rc::clone(&shared));
    world
        .non_send_mut::<Rc<RefCell<Vec<i32>>>>()
        .borrow_mut()
        .push(3);
    assert_eq!(*shared.borrow(), [1, 2, 3]);
    assert!(world.contains_non_send::<Rc<RefCell<Vec<i32>>>>());
}

#[test]
fn non_send_system_params_run_on_the_calling_thread() {
    let mut app = app();
    let calling = std::thread::current().id();
    app.world_mut()
        .insert_non_send(Rc::new(RefCell::new(None::<std::thread::ThreadId>)));
    update(&mut app).add_systems(
        |slot: NonSend<Rc<RefCell<Option<std::thread::ThreadId>>>>| {
            *slot.borrow_mut() = Some(std::thread::current().id());
        },
    );
    app.tick(DT).unwrap();
    let slot = app
        .world()
        .non_send::<Rc<RefCell<Option<std::thread::ThreadId>>>>();
    assert_eq!(*slot.borrow(), Some(calling));
}

// ---- system-level behaviour ---------------------------------------------------

#[test]
fn systems_can_read_and_write_resources_across_ticks() {
    let mut app = app();
    app.world_mut().init_resource::<Gold>();
    update(&mut app)
        .add_systems(|mut gold: ResMut<Gold>, time: Res<Time>| gold.0 += time.tick as i64);
    app.run_fixed(10, DT).unwrap();
    assert_eq!(app.world().resource::<Gold>().0, 55);
}

#[test]
fn optional_resource_params_see_late_insertions() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(|gold: Option<Res<Gold>>, mut seen: ResMut<Seen>| {
        seen.0.push(gold.map_or(-1, |g| g.0));
    });
    app.tick(DT).unwrap();
    app.world_mut().insert_resource(Gold(3));
    app.tick(DT).unwrap();
    app.world_mut().remove_resource::<Gold>();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [-1, 3, -1]);
}

#[test]
fn run_if_resource_exists_gates_a_system() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app)
        .add_systems((|mut c: ResMut<Counter>| c.0 += 1).run_if(resource_exists::<Gold>));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 0);
    app.world_mut().insert_resource(Gold(0));
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn run_if_resource_equals_gates_on_current_value() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().insert_resource(Gold(1));
    update(&mut app)
        .add_systems((|mut c: ResMut<Counter>| c.0 += 1).run_if(resource_equals(Gold(2))));
    app.tick(DT).unwrap();
    app.world_mut().resource_mut::<Gold>().0 = 2;
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().resource_mut::<Gold>().0 = 3;
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn resource_changed_condition_fires_once_per_mutation() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().insert_resource(Gold(0));
    update(&mut app)
        .add_systems((|mut c: ResMut<Counter>| c.0 += 1).run_if(resource_changed::<Gold>));
    app.tick(DT).unwrap(); // initial insertion is a change
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().resource_mut::<Gold>().0 = 5;
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn resource_added_condition_fires_for_each_insertion_after_removal() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    update(&mut app)
        .add_systems((|mut c: ResMut<Counter>| c.0 += 1).run_if(resource_added::<Gold>));
    app.tick(DT).unwrap();
    app.world_mut().insert_resource(Gold(0));
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().remove_resource::<Gold>();
    app.world_mut().insert_resource(Gold(1));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 2);
}

#[test]
fn resource_removed_condition_fires_after_removal() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().insert_resource(Gold(1));
    update(&mut app)
        .add_systems((|mut c: ResMut<Counter>| c.0 += 1).run_if(resource_removed::<Gold>));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 0);
    app.world_mut().remove_resource::<Gold>();
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 1);
}

#[test]
fn res_is_changed_only_after_a_write_since_the_systems_last_run() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.world_mut().insert_resource(Gold(0));
    update(&mut app).add_systems(|gold: Res<Gold>, mut seen: ResMut<Seen>| {
        seen.0.push(i64::from(gold.is_changed()))
    });
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.world_mut().resource_mut::<Gold>().0 += 1;
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0, 1, 0]);
}

#[test]
fn res_mut_bypass_change_detection_suppresses_the_flag() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    app.world_mut().insert_resource(Gold(0));
    update(&mut app).add_systems(
        (
            |mut gold: ResMut<Gold>, time: Res<Time>| {
                if time.tick > 1 {
                    gold.bypass_change_detection().0 += 1;
                }
            },
            |gold: Res<Gold>, mut seen: ResMut<Seen>| seen.0.push(i64::from(gold.is_changed())),
        )
            .chain(),
    );
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [1, 0, 0]);
    assert_eq!(app.world().resource::<Gold>().0, 2);
}

#[test]
fn commands_insert_and_remove_resources_with_deferred_visibility() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(
        (
            |mut commands: Commands, time: Res<Time>| match time.tick {
                1 => commands.insert_resource(Gold(8)),
                3 => commands.remove_resource::<Gold>(),
                _ => {}
            },
            |gold: Option<Res<Gold>>, mut seen: ResMut<Seen>| {
                seen.0.push(gold.map_or(-1, |g| g.0));
            },
        )
            .chain(),
    );
    for _ in 0..4 {
        app.tick(DT).unwrap();
    }
    assert_eq!(app.world().resource::<Seen>().0, [8, 8, -1, -1]);
}

#[test]
fn local_state_persists_per_system_across_ticks() {
    let mut app = app();
    app.world_mut().init_resource::<Seen>();
    update(&mut app).add_systems(
        (
            |mut n: Local<i64>, mut seen: ResMut<Seen>| {
                *n += 2;
                seen.0.push(*n);
            },
            |mut n: Local<i64>, mut seen: ResMut<Seen>| {
                *n += 1000;
                seen.0.push(*n);
            },
        )
            .chain(),
    );
    app.run_fixed(3, DT).unwrap();
    assert_eq!(
        app.world().resource::<Seen>().0,
        [2, 1000, 4, 2000, 6, 3000]
    );
}

#[test]
fn bank_transfers_between_resources_conserve_total_over_many_ticks() {
    #[derive(Resource)]
    struct Rand(Rng);
    let mut app = app();
    app.world_mut().insert_resource(Gold(1_000));
    app.world_mut().insert_resource(Bank(500));
    app.world_mut().insert_resource(Rand(Rng::new(2024)));
    update(&mut app).add_systems(
        |mut rng: ResMut<Rand>, mut gold: ResMut<Gold>, mut bank: ResMut<Bank>| {
            let amount = rng.0.below(300) as i64;
            if rng.0.chance(1, 2) {
                let moved = amount.min(gold.0);
                gold.0 -= moved;
                bank.0 += moved;
            } else {
                let moved = amount.min(bank.0);
                bank.0 -= moved;
                gold.0 += moved;
            }
            assert_eq!(gold.0 + bank.0, 1_500);
        },
    );
    app.run_fixed(400, DT).unwrap();
    let (g, b) = (
        app.world().resource::<Gold>().0,
        app.world().resource::<Bank>().0,
    );
    assert_eq!(g + b, 1_500);
    assert!(g >= 0 && b >= 0);
}

#[test]
fn conflicting_resource_access_without_ordering_is_rejected_in_the_engine_schedule() {
    let mut app = app();
    app.world_mut().init_resource::<Gold>();
    update(&mut app).add_systems((
        |mut g: ResMut<Gold>| g.0 += 1,
        |g: Res<Gold>| {
            let _ = g.0;
        },
    ));
    assert!(app.tick(DT).is_err());
    assert_eq!(app.world().resource::<Gold>().0, 0);
}

#[test]
fn shared_resource_readers_run_without_ordering_requirements() {
    let mut app = app();
    app.world_mut().insert_resource(Gold(3));
    app.world_mut().init_resource::<Seen>();
    app.world_mut().init_resource::<Counter>();
    update(&mut app).add_systems((
        |g: Res<Gold>, mut seen: ResMut<Seen>| seen.0.push(g.0),
        |g: Res<Gold>, mut c: ResMut<Counter>| c.0 += g.0,
    ));
    app.tick(DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [3]);
    assert_eq!(app.world().resource::<Counter>().0, 3);
}
