//! Buffered values and render extraction driven directly on a `World` and
//! `Schedule`, without the `EngineApp` lifecycle.
mod common;

use common::*;
use engine_ecs::prelude::*;

#[test]
fn buffered_bundle_spawns_current_and_next_equal() {
    let mut world = World::new();
    let e = world.spawn(Buffered::new(5_i32)).id();
    assert_eq!(world.get::<Current<i32>>(e), Some(&Current(5)));
    assert_eq!(world.get::<Next<i32>>(e), Some(&Next(5)));
    let from: Buffered<&str> = "x".into();
    assert_eq!(from, Buffered::new("x"));
}

#[test]
fn publish_buffered_copies_only_differing_values() {
    let mut world = World::new();
    let e = world.spawn(Buffered::new(1_i32)).id();
    world.get_mut::<Next<i32>>(e).unwrap().0 = 9;
    let mut schedule = Schedule::default();
    schedule.add_systems(publish_buffered::<i32>);
    schedule.run(&mut world);
    assert_eq!(world.get::<Current<i32>>(e), Some(&Current(9)));
}

#[test]
fn publish_does_not_mark_current_changed_when_equal() {
    #[derive(Resource, Default)]
    struct Changes(usize);

    let mut world = World::new();
    world.init_resource::<Changes>();
    world.spawn(Buffered::new(3_i32));
    let mut schedule = Schedule::default();
    schedule.add_systems(
        (
            publish_buffered::<i32>,
            |changed: Query<(), Changed<Current<i32>>>, mut count: ResMut<Changes>| {
                count.0 += changed.iter().count();
            },
        )
            .chain(),
    );
    schedule.run(&mut world); // first run sees the spawn as added/changed
    let after_spawn = world.resource::<Changes>().0;
    schedule.run(&mut world); // nothing differs, so nothing changes
    assert_eq!(world.resource::<Changes>().0, after_spawn);
}

fn extraction_schedule() -> Schedule {
    let mut schedule = Schedule::default();
    schedule
        .configure_sets((ExtractionSet::Cleanup, ExtractionSet::Components).chain())
        .add_systems(engine_ecs::cleanup_extracted_entities.in_set(ExtractionSet::Cleanup))
        .add_systems(extract_component::<Pos>.in_set(ExtractionSet::Components));
    schedule
}

#[test]
fn extraction_copies_changes_and_maps_identity() {
    let mut world = World::new();
    world.init_resource::<RenderWorld>();
    let src = world.spawn(Pos { x: 1, y: 2 }).id();
    let mut schedule = extraction_schedule();
    schedule.run(&mut world);
    let render = world.resource::<RenderWorld>();
    let dst = render.entity_for(src).expect("mapped");
    assert_eq!(render.world().get::<Pos>(dst), Some(&Pos { x: 1, y: 2 }));
    assert_eq!(render.world().get::<ExtractedFrom>(dst).unwrap().0, src);
}

#[test]
fn extraction_propagates_component_removal_and_despawn() {
    let mut world = World::new();
    world.init_resource::<RenderWorld>();
    let a = world.spawn(Pos::default()).id();
    let b = world.spawn(Pos::default()).id();
    let mut schedule = extraction_schedule();
    schedule.run(&mut world);
    world.clear_trackers();
    world.entity_mut(a).remove::<Pos>();
    world.despawn(b);
    schedule.run(&mut world);
    let render = world.resource::<RenderWorld>();
    let dst_a = render.entity_for(a).expect("entity kept");
    assert!(render.world().get::<Pos>(dst_a).is_none());
    assert!(render.entity_for(b).is_none());
}

#[test]
fn entity_or_spawn_is_stable() {
    let mut render = RenderWorld::default();
    let source = Entity::from_raw_u32(42).unwrap();
    let first = render.entity_or_spawn(source);
    assert_eq!(render.entity_or_spawn(source), first);
    assert_eq!(render.entity_for(source), Some(first));
}
