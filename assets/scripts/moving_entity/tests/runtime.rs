use engine_core::bevy_ecs::prelude::Entity;
use engine_project::{ActionFrame, Project, Runtime, Transform};
use moving_entity_gameplay::{STARTED_MESSAGE, configure};

const DT: f32 = 1.0 / 60.0;

fn runtime() -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project.json")).unwrap())
            .unwrap();
    configure(runtime.app_mut()).unwrap();
    runtime
}
fn free_entities(runtime: &Runtime) -> Vec<Entity> {
    let owner = runtime.entity("entity:mover-owner").unwrap();
    let mut world_entities: Vec<_> = runtime
        .app()
        .world()
        .iter_entities()
        .filter(|e| e.contains::<Transform>() && e.id() != owner)
        .map(|e| e.id())
        .collect();
    world_entities.sort();
    world_entities
}
fn x_of(runtime: &Runtime, entity: Entity) -> f32 {
    runtime
        .app()
        .world()
        .get::<Transform>(entity)
        .unwrap()
        .position[0]
}

#[test]
fn start_creates_one_free_entity_at_origin_and_logs() {
    let mut runtime = runtime();
    runtime.start().unwrap();
    let free = free_entities(&runtime);
    assert_eq!(free.len(), 1);
    assert_eq!(
        runtime
            .app()
            .world()
            .get::<Transform>(free[0])
            .unwrap()
            .position,
        [0.0; 3]
    );
    assert!(runtime.snapshot().logs.iter().any(|l| l == STARTED_MESSAGE));
    runtime.stop().unwrap();
}
#[test]
fn each_update_advances_x_by_the_frame_delta() {
    let mut runtime = runtime();
    runtime.tick(0.25, ActionFrame::default()).unwrap();
    let entity = free_entities(&runtime)[0];
    assert!((x_of(&runtime, entity) - 0.25).abs() < 1e-6);
    runtime.tick(0.5, ActionFrame::default()).unwrap();
    assert!((x_of(&runtime, entity) - 0.75).abs() < 1e-6);
    runtime.stop().unwrap();
}
#[test]
fn one_second_of_fixed_steps_moves_one_unit() {
    let mut runtime = runtime();
    for _ in 0..60 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    let entity = free_entities(&runtime)[0];
    assert!((x_of(&runtime, entity) - 1.0).abs() < 1e-4);
    runtime.stop().unwrap();
}
#[test]
fn y_and_z_are_never_modified() {
    let mut runtime = runtime();
    runtime.start().unwrap();
    let entity = free_entities(&runtime)[0];
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .position = [0.0, 2.5, -7.0];
    for _ in 0..10 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    let position = runtime
        .app()
        .world()
        .get::<Transform>(entity)
        .unwrap()
        .position;
    assert_eq!((position[1], position[2]), (2.5, -7.0));
    assert!(position[0] > 0.0);
    runtime.stop().unwrap();
}
#[test]
fn movement_is_deterministic_across_runs() {
    let run = || {
        let mut runtime = runtime();
        for step in 1..=37 {
            runtime
                .tick(DT * (1 + step % 3) as f32, ActionFrame::default())
                .unwrap();
        }
        let entity = free_entities(&runtime)[0];
        let x = x_of(&runtime, entity);
        runtime.stop().unwrap();
        x
    };
    assert_eq!(run().to_bits(), run().to_bits());
}
#[test]
fn input_does_not_affect_the_sample() {
    let mut runtime = runtime();
    let mut input = ActionFrame::default();
    input.pressed.insert("move_left".into());
    runtime.tick(0.5, input).unwrap();
    let entity = free_entities(&runtime)[0];
    assert!((x_of(&runtime, entity) - 0.5).abs() < 1e-6);
    runtime.stop().unwrap();
}
#[test]
fn stop_releases_the_created_entity() {
    let mut runtime = runtime();
    runtime.tick(DT, ActionFrame::default()).unwrap();
    let entity = free_entities(&runtime)[0];
    runtime.stop().unwrap();
    assert!(runtime.app().world().get_entity(entity).is_err());
}
#[test]
fn dead_entity_is_ignored_by_updates_and_teardown() {
    let mut runtime = runtime();
    runtime.tick(DT, ActionFrame::default()).unwrap();
    let entity = free_entities(&runtime)[0];
    runtime.app_mut().world_mut().despawn(entity);
    runtime.tick(DT, ActionFrame::default()).unwrap();
    runtime.stop().unwrap();
}
#[test]
fn authored_owner_is_untouched() {
    let mut runtime = runtime();
    for _ in 0..5 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    assert_eq!(runtime.position("entity:mover-owner").unwrap(), [0.0; 3]);
    runtime.stop().unwrap();
}
#[test]
fn invalid_delta_is_rejected_by_the_runtime() {
    let mut runtime = runtime();
    assert!(runtime.tick(0.0, ActionFrame::default()).is_err());
}
