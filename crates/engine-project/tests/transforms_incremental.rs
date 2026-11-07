//! `refresh_world_transforms` recomputes only what changed: the entities whose local `Transform`
//! or `Parent` changed since the previous refresh and their descendants. These tests pin that
//! the stored `WorldTransform`s stay exactly what a full recompute would produce.
use bevy_ecs::change_detection::Tick;
use engine_project::{ActionFrame, Parent, Project, Runtime, Transform, WorldTransform};
use serde_json::json;
use std::fs;

const DT: f32 = 1.0 / 60.0;

fn runtime() -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
            {"id":"a","name":"A","components":{"Transform":{"position":[1,0,0]}}},
            {"id":"b","name":"B","parent":"a","components":{"Transform":{"position":[0,2,0]}}},
            {"id":"c","name":"C","parent":"b","components":{"Transform":{"position":[0,0,3]}}},
            {"id":"free","name":"Free","components":{"Transform":{"position":[5,0,0]}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"transforms","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    runtime.tick(DT, ActionFrame::default()).unwrap();
    (dir, runtime)
}

fn stored(runtime: &Runtime, id: &str) -> [f32; 3] {
    let entity = runtime.entity(id).unwrap();
    runtime
        .app()
        .world()
        .get::<WorldTransform>(entity)
        .unwrap()
        .position()
}

/// A point in the world's change history, to ask later which `WorldTransform`s were written
/// after it.
fn mark(runtime: &Runtime) -> Tick {
    runtime.app().world().read_change_tick()
}

fn written_since(runtime: &Runtime, id: &str, since: Tick) -> bool {
    let entity = runtime.entity(id).unwrap();
    let world = runtime.app().world();
    world
        .entity(entity)
        .get_change_ticks::<WorldTransform>()
        .unwrap()
        .is_changed(since, world.read_change_tick())
}

fn tick(runtime: &mut Runtime) {
    runtime.tick(DT, ActionFrame::default()).unwrap();
}

fn edit<R>(runtime: &mut Runtime, id: &str, f: impl FnOnce(&mut Transform) -> R) -> R {
    let entity = runtime.entity(id).unwrap();
    let mut transform = runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(entity)
        .unwrap();
    f(&mut transform)
}

#[test]
fn the_initial_refresh_resolves_every_hierarchy() {
    let (_dir, runtime) = runtime();
    assert_eq!(stored(&runtime, "a"), [1.0, 0.0, 0.0]);
    assert_eq!(stored(&runtime, "b"), [1.0, 2.0, 0.0]);
    assert_eq!(stored(&runtime, "c"), [1.0, 2.0, 3.0]);
    assert_eq!(stored(&runtime, "free"), [5.0, 0.0, 0.0]);
}

#[test]
fn moving_a_parent_moves_its_unchanged_descendants_and_nothing_else() {
    let (_dir, mut runtime) = runtime();
    let since = mark(&runtime);
    edit(&mut runtime, "a", |t| t.position = [10.0, 0.0, 0.0]);
    tick(&mut runtime);
    assert_eq!(stored(&runtime, "a"), [10.0, 0.0, 0.0]);
    assert_eq!(stored(&runtime, "b"), [10.0, 2.0, 0.0]);
    assert_eq!(stored(&runtime, "c"), [10.0, 2.0, 3.0]);
    assert!(
        !written_since(&runtime, "free", since),
        "an unrelated entity is not rewritten"
    );
    assert!(written_since(&runtime, "c", since));
}

#[test]
fn a_tick_without_changes_rewrites_nothing() {
    let (_dir, mut runtime) = runtime();
    let since = mark(&runtime);
    tick(&mut runtime);
    tick(&mut runtime);
    for id in ["a", "b", "c", "free"] {
        assert!(!written_since(&runtime, id, since), "{id} was rewritten");
    }
}

#[test]
fn a_changed_leaf_leaves_its_ancestors_alone() {
    let (_dir, mut runtime) = runtime();
    let since = mark(&runtime);
    edit(&mut runtime, "c", |t| t.position = [0.0, 0.0, 9.0]);
    tick(&mut runtime);
    assert_eq!(stored(&runtime, "c"), [1.0, 2.0, 9.0]);
    assert!(!written_since(&runtime, "a", since));
    assert!(!written_since(&runtime, "b", since));
}

#[test]
fn removing_a_parent_component_makes_the_entity_a_root() {
    let (_dir, mut runtime) = runtime();
    let b = runtime.entity("b").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(b)
        .remove::<Parent>();
    tick(&mut runtime);
    assert_eq!(stored(&runtime, "b"), [0.0, 2.0, 0.0]);
    assert_eq!(
        stored(&runtime, "c"),
        [0.0, 2.0, 3.0],
        "descendants follow the new root"
    );
}

#[test]
fn reparenting_recomputes_the_entity_and_its_descendants() {
    let (_dir, mut runtime) = runtime();
    let c = runtime.entity("c").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(c)
        .insert(Parent("free".into()));
    tick(&mut runtime);
    assert_eq!(stored(&runtime, "c"), [5.0, 0.0, 3.0]);
}

#[test]
fn losing_the_transform_removes_the_world_transform() {
    let (_dir, mut runtime) = runtime();
    let free = runtime.entity("free").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(free)
        .remove::<Transform>();
    tick(&mut runtime);
    assert!(runtime.app().world().get::<WorldTransform>(free).is_none());
}

#[test]
fn a_transform_added_later_gets_a_world_transform() {
    let (_dir, mut runtime) = runtime();
    let free = runtime.entity("free").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(free)
        .remove::<Transform>();
    tick(&mut runtime);
    runtime.app_mut().world_mut().entity_mut(free).insert((
        Transform {
            position: [7.0, 0.0, 0.0],
            ..Default::default()
        },
        Parent("a".into()),
    ));
    tick(&mut runtime);
    assert_eq!(stored(&runtime, "free"), [8.0, 0.0, 0.0]);
}

#[test]
fn a_parent_that_lost_its_transform_still_fails_the_tick_for_its_children() {
    let (_dir, mut runtime) = runtime();
    let a = runtime.entity("a").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .entity_mut(a)
        .remove::<Transform>();
    let error = runtime.tick(DT, ActionFrame::default()).unwrap_err();
    assert!(error.to_string().contains("requires Transform"), "{error}");
}

#[test]
fn an_invalid_local_transform_fails_the_tick() {
    let (_dir, mut runtime) = runtime();
    edit(&mut runtime, "free", |t| t.scale = [0.0, 1.0, 1.0]);
    let error = runtime.tick(DT, ActionFrame::default()).unwrap_err();
    assert!(error.to_string().contains("nonsingular"), "{error}");
}
