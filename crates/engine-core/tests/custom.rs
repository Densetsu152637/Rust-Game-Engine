//! User-defined components, labels, systems, schedules and hierarchies.
mod common;

use common::*;
use engine_core::bevy_ecs::schedule::ScheduleLabel;
use engine_core::prelude::*;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct Group<T: Send + Sync + 'static + std::fmt::Debug + Clone + PartialEq + Eq + std::hash::Hash>(
    T,
);

#[derive(Component, Debug, Clone, PartialEq)]
struct Meters(f32);
#[derive(Component, Debug, Clone, PartialEq)]
struct Seconds(f32);

trait Scale {
    fn scale(&mut self, by: f32);
}
impl Scale for Meters {
    fn scale(&mut self, by: f32) {
        self.0 *= by;
    }
}
impl Scale for Seconds {
    fn scale(&mut self, by: f32) {
        self.0 *= by;
    }
}

fn scale_all<T: Component<Mutability = engine_core::bevy_ecs::component::Mutable> + Scale>(
    mut q: Query<&mut T>,
) {
    for mut value in &mut q {
        value.scale(2.0);
    }
}

#[test]
fn generic_systems_instantiate_independently_per_component_type() {
    let mut app = app();
    let e = app.world_mut().spawn((Meters(1.5), Seconds(0.25))).id();
    update(&mut app).add_systems((scale_all::<Meters>, scale_all::<Seconds>));
    app.run_fixed(3, DT).unwrap();
    assert_eq!(app.world().get::<Meters>(e), Some(&Meters(12.0)));
    assert_eq!(app.world().get::<Seconds>(e), Some(&Seconds(2.0)));
}

#[test]
fn generic_instances_of_the_same_function_conflict_only_on_shared_data() {
    let mut app = app();
    app.world_mut().spawn(Meters(1.0));
    update(&mut app).add_systems((scale_all::<Meters>, scale_all::<Meters>));
    assert!(app.tick(DT).is_err());
}

#[test]
fn data_carrying_set_labels_distinguish_values_and_types() {
    let mut app = app();
    app.world_mut().init_resource::<Log>();
    update(&mut app)
        .configure_sets(Group("b").after(Group("a")))
        .configure_sets(Group(1_u8).before(Group("a")))
        .add_systems((
            (|mut l: ResMut<Log>| l.0.push("b")).in_set(Group("b")),
            (|mut l: ResMut<Log>| l.0.push("a")).in_set(Group("a")),
            (|mut l: ResMut<Log>| l.0.push("one")).in_set(Group(1_u8)),
        ));
    app.tick(DT).unwrap();
    assert_eq!(log(&app), ["one", "a", "b"]);
}

#[test]
fn custom_schedule_label_can_be_substepped_from_an_exclusive_system() {
    #[derive(ScheduleLabel, Debug, Clone, PartialEq, Eq, Hash)]
    struct Physics;
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    app.world_mut().init_resource::<Seen>();
    let mut physics = Schedule::new(Physics);
    physics.add_systems(|mut c: ResMut<Counter>| c.0 += 1);
    app.world_mut().add_schedule(physics);
    update(&mut app).add_systems(|world: &mut World| {
        for _ in 0..4 {
            world.run_schedule(Physics);
        }
        let c = world.resource::<Counter>().0;
        world.resource_mut::<Seen>().0.push(c);
    });
    app.run_fixed(3, DT).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [4, 8, 12]);
}

#[test]
fn user_resources_derived_from_time_stay_consistent() {
    #[derive(Resource, Default)]
    struct Clock {
        scaled: f64,
        scale: f64,
    }
    let mut app = app();
    app.world_mut().insert_resource(Clock {
        scaled: 0.0,
        scale: 2.0,
    });
    update(&mut app).add_systems(|t: Res<Time>, mut c: ResMut<Clock>| {
        let scale = c.scale;
        c.scaled += f64::from(t.dt) * scale;
    });
    app.run_fixed(8, 0.25).unwrap();
    assert_eq!(app.world().resource::<Clock>().scaled, 4.0);
    assert_eq!(app.time().elapsed * 2.0, 4.0);
}

#[test]
fn parent_child_hierarchy_despawns_children_with_the_parent() {
    let mut app = app();
    let parent = app.world_mut().spawn(Health(1)).id();
    let kids: Vec<_> = (0..3)
        .map(|i| {
            app.world_mut()
                .spawn((Health(10 + i), ChildOf(parent)))
                .id()
        })
        .collect();
    let grandchild = app.world_mut().spawn((Tag, ChildOf(kids[0]))).id();
    let bystander = app.world_mut().spawn(Health(99)).id();
    assert_eq!(app.world().get::<Children>(parent).unwrap().len(), 3);
    app.world_mut().entity_mut(parent).despawn();
    app.tick(DT).unwrap();
    for e in kids.iter().chain([&parent, &grandchild]) {
        assert!(app.world().get_entity(*e).is_err(), "{e:?} should be gone");
    }
    assert_eq!(app.world().get::<Health>(bystander), Some(&Health(99)));
}

#[test]
fn detaching_a_child_removes_it_from_the_parents_children() {
    let mut world = World::new();
    let parent = world.spawn_empty().id();
    let a = world.spawn(ChildOf(parent)).id();
    let b = world.spawn(ChildOf(parent)).id();
    world.entity_mut(a).remove::<ChildOf>();
    let kids: Vec<Entity> = world.get::<Children>(parent).unwrap().iter().collect();
    assert_eq!(kids, [b]);
}

#[test]
fn user_bundles_combine_with_required_components_and_extraction() {
    #[derive(Component, Clone, Debug, PartialEq, Default)]
    #[require(Tag)]
    struct Sprite(u32);
    #[derive(Bundle)]
    struct Actor {
        sprite: Sprite,
        pos: Pos,
    }
    let mut app = app();
    app.extract_component::<Sprite>();
    let e = app
        .world_mut()
        .spawn(Actor {
            sprite: Sprite(9),
            pos: Pos { x: 1, y: 2 },
        })
        .id();
    app.tick(DT).unwrap();
    assert!(app.world().get::<Tag>(e).is_some());
    let render = app.render_snapshot().entity_for(e).unwrap();
    assert_eq!(app.render_world().get::<Sprite>(render), Some(&Sprite(9)));
    assert!(app.render_world().get::<Pos>(render).is_none());
}

#[test]
fn one_shot_systems_registered_by_users_run_inside_stages() {
    let mut app = app();
    app.world_mut().init_resource::<Counter>();
    let id = app
        .world_mut()
        .register_system(|mut c: ResMut<Counter>| c.0 += 10);
    update(&mut app).add_systems(move |mut commands: Commands| commands.run_system(id));
    app.run_fixed(4, DT).unwrap();
    assert_eq!(app.world().resource::<Counter>().0, 40);
}

#[test]
fn user_defined_marker_state_machines_transition_through_commands() {
    #[derive(Component)]
    struct Idle;
    #[derive(Component)]
    struct Running(u32);
    #[derive(Component)]
    struct Done;
    fn machine(ticks: u64) -> (EngineApp, Entity) {
        let mut app = app();
        let e = app.world_mut().spawn(Idle).id();
        update(&mut app).add_systems(
            (
                |mut c: Commands, q: Query<Entity, With<Idle>>| {
                    for e in &q {
                        c.entity(e).remove::<Idle>().insert(Running(0));
                    }
                },
                |mut c: Commands, mut q: Query<(Entity, &mut Running)>| {
                    for (e, mut r) in &mut q {
                        r.0 += 1;
                        if r.0 == 3 {
                            c.entity(e).remove::<Running>().insert(Done);
                        }
                    }
                },
            )
                .chain(),
        );
        app.run_fixed(ticks, DT).unwrap();
        (app, e)
    }
    let (early, e) = machine(1);
    assert_eq!(early.world().get::<Running>(e).map(|r| r.0), Some(1));
    let (app2, e2) = machine(3);
    assert!(app2.world().get::<Done>(e2).is_some());
    assert!(app2.world().get::<Running>(e2).is_none());
}
