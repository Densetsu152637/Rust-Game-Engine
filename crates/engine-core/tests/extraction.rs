//! Change-driven render extraction and dirty tracking.
mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use common::*;
use engine_core::prelude::*;

#[derive(Component, Debug, Clone, PartialEq)]
struct Mesh(u32);
#[derive(Component, Debug, Clone, PartialEq)]
struct Tint(u32);

#[derive(Component)]
struct Counted {
    value: u32,
    clones: Arc<AtomicUsize>,
}
impl Clone for Counted {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::SeqCst);
        Self {
            value: self.value,
            clones: Arc::clone(&self.clones),
        }
    }
}

fn extraction_app() -> EngineApp {
    let mut app = app();
    app.extract_component::<Mesh>();
    app.extract_component::<Tint>();
    app
}

fn render_value<T: Component + Clone>(app: &EngineApp, source: Entity) -> Option<T> {
    let render = app.render_snapshot().entity_for(source)?;
    app.render_world().get::<T>(render).cloned()
}

#[test]
fn first_tick_copies_every_extracted_component_to_the_render_world() {
    let mut app = extraction_app();
    let ids: Vec<_> = (0..10)
        .map(|i| app.world_mut().spawn(Mesh(i)).id())
        .collect();
    app.tick(DT).unwrap();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(render_value::<Mesh>(&app, *id), Some(Mesh(i as u32)));
    }
    assert_eq!(count::<Mesh>(app.render_world_mut()), 10);
}

#[test]
fn entities_without_extracted_components_get_no_render_entity() {
    let mut app = extraction_app();
    let plain = app.world_mut().spawn(Health(3)).id();
    let shown = app.world_mut().spawn((Health(3), Mesh(1))).id();
    app.tick(DT).unwrap();
    assert!(app.render_snapshot().entity_for(plain).is_none());
    assert!(app.render_snapshot().entity_for(shown).is_some());
}

#[test]
fn unregistered_component_types_are_never_extracted() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn((Mesh(1), Health(9))).id();
    app.tick(DT).unwrap();
    assert!(render_value::<Health>(&app, e).is_none());
}

#[test]
fn render_entities_record_their_simulation_source() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    let render = app.render_snapshot().entity_for(e).unwrap();
    assert_eq!(
        app.render_world().get::<ExtractedFrom>(render).unwrap().0,
        e
    );
}

#[test]
fn render_and_simulation_entity_handles_are_independent_identities() {
    let mut app = extraction_app();
    for _ in 0..5 {
        app.world_mut().spawn(Health(0));
    }
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    let render = app.render_snapshot().entity_for(e).unwrap();
    assert_ne!(app.world().id(), app.render_world().id());
    assert!(app.render_world().get_entity(render).is_ok());
    assert_eq!(app.world().get::<Mesh>(e), Some(&Mesh(1)));
}

#[test]
fn unchanged_components_are_not_cloned_again() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut app = app();
    app.extract_component::<Counted>();
    for value in 0..8 {
        app.world_mut().spawn(Counted {
            value,
            clones: Arc::clone(&clones),
        });
    }
    app.run_fixed(12, DT).unwrap();
    assert_eq!(clones.load(Ordering::SeqCst), 8);
}

#[test]
fn only_modified_components_are_cloned_on_later_ticks() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut app = app();
    app.extract_component::<Counted>();
    let ids: Vec<_> = (0..6)
        .map(|value| {
            app.world_mut()
                .spawn(Counted {
                    value,
                    clones: Arc::clone(&clones),
                })
                .id()
        })
        .collect();
    app.tick(DT).unwrap();
    for id in [ids[1], ids[4]] {
        app.world_mut().get_mut::<Counted>(id).unwrap().value += 100;
    }
    app.tick(DT).unwrap();
    app.tick(DT).unwrap();
    assert_eq!(clones.load(Ordering::SeqCst), 6 + 2);
    assert_eq!(render_value_counted(&app, ids[4]), 104);
    assert_eq!(render_value_counted(&app, ids[0]), 0);
}

fn render_value_counted(app: &EngineApp, source: Entity) -> u32 {
    let render = app.render_snapshot().entity_for(source).unwrap();
    app.render_world().get::<Counted>(render).unwrap().value
}

#[test]
fn modification_is_invisible_to_render_until_the_next_extraction() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    app.world_mut().get_mut::<Mesh>(e).unwrap().0 = 2;
    assert_eq!(render_value::<Mesh>(&app, e), Some(Mesh(1)));
    app.tick(DT).unwrap();
    assert_eq!(render_value::<Mesh>(&app, e), Some(Mesh(2)));
}

#[test]
fn simulation_changes_in_update_reach_render_in_the_same_tick() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(0)).id();
    update(&mut app).add_systems(|mut q: Query<&mut Mesh>| {
        for mut m in &mut q {
            m.0 += 1;
        }
    });
    app.run_fixed(5, DT).unwrap();
    assert_eq!(render_value::<Mesh>(&app, e), Some(Mesh(5)));
}

#[test]
fn render_systems_observe_the_extracted_snapshot() {
    let mut app = extraction_app();
    app.render_world_mut().init_resource::<Seen>();
    app.world_mut().spawn(Mesh(7));
    app.world_mut().spawn(Mesh(9));
    app.schedule_mut(Stage::Render)
        .add_systems(|q: Query<&Mesh>, mut s: ResMut<Seen>| {
            let mut v: Vec<i64> = q.iter().map(|m| i64::from(m.0)).collect();
            v.sort();
            s.0.extend(v);
        });
    app.tick(DT).unwrap();
    assert_eq!(app.render_world().resource::<Seen>().0, [7, 9]);
}

#[test]
fn component_removal_propagates_but_keeps_the_render_entity() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn((Mesh(1), Tint(2))).id();
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(e).remove::<Mesh>();
    app.tick(DT).unwrap();
    assert!(render_value::<Mesh>(&app, e).is_none());
    assert_eq!(render_value::<Tint>(&app, e), Some(Tint(2)));
}

#[test]
fn despawn_removes_the_render_entity_and_its_mapping() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    let render = app.render_snapshot().entity_for(e).unwrap();
    app.world_mut().despawn(e);
    app.tick(DT).unwrap();
    assert!(app.render_snapshot().entity_for(e).is_none());
    assert!(app.render_world().get_entity(render).is_err());
}

#[test]
fn replacement_entity_gets_a_fresh_render_entity() {
    let mut app = extraction_app();
    let old = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    app.world_mut().despawn(old);
    let new = app.world_mut().spawn(Mesh(2)).id();
    app.tick(DT).unwrap();
    assert_ne!(old, new);
    assert!(app.render_snapshot().entity_for(old).is_none());
    assert_eq!(render_value::<Mesh>(&app, new), Some(Mesh(2)));
}

#[test]
fn remove_and_reinsert_in_one_tick_ends_with_the_new_value() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    app.world_mut().entity_mut(e).remove::<Mesh>();
    app.world_mut().entity_mut(e).insert(Mesh(42));
    app.tick(DT).unwrap();
    assert_eq!(render_value::<Mesh>(&app, e), Some(Mesh(42)));
}

#[test]
fn component_added_to_an_existing_entity_later_is_extracted_onto_the_same_render_entity() {
    let mut app = extraction_app();
    let e = app.world_mut().spawn(Mesh(1)).id();
    app.tick(DT).unwrap();
    let render_before = app.render_snapshot().entity_for(e).unwrap();
    app.world_mut().entity_mut(e).insert(Tint(5));
    app.tick(DT).unwrap();
    assert_eq!(app.render_snapshot().entity_for(e), Some(render_before));
    assert_eq!(render_value::<Tint>(&app, e), Some(Tint(5)));
    assert_eq!(count::<ExtractedFrom>(app.render_world_mut()), 1);
}

#[test]
fn registering_the_same_component_twice_extracts_it_once() {
    let clones = Arc::new(AtomicUsize::new(0));
    let mut app = app();
    app.extract_component::<Counted>()
        .extract_component::<Counted>();
    app.world_mut().spawn(Counted {
        value: 1,
        clones: Arc::clone(&clones),
    });
    app.run_fixed(3, DT).unwrap();
    assert_eq!(clones.load(Ordering::SeqCst), 1);
}

#[test]
fn render_snapshot_is_not_exposed_as_a_resource_between_ticks() {
    let mut app = extraction_app();
    app.world_mut().spawn(Mesh(1));
    for _ in 0..3 {
        app.tick(DT).unwrap();
        assert!(!app.world().contains_resource::<RenderWorld>());
    }
}

#[test]
fn render_world_resources_persist_across_ticks() {
    let mut app = extraction_app();
    app.render_world_mut().init_resource::<Counter>();
    app.schedule_mut(Stage::Render)
        .add_systems(|mut c: ResMut<Counter>| c.0 += 1);
    app.run_fixed(9, DT).unwrap();
    assert_eq!(app.render_world().resource::<Counter>().0, 9);
}

#[test]
fn entity_or_spawn_is_idempotent_and_replaces_dead_render_entities() {
    let mut app = app();
    let source = app.world_mut().spawn_empty().id();
    app.world_mut().insert_resource(Handle(source));
    app.schedule_mut(Stage::Extract).add_systems(
        (|h: Res<Handle>, mut r: ResMut<RenderWorld>, mut s: ResMut<Seen>| {
            let a = r.entity_or_spawn(h.0);
            let b = r.entity_or_spawn(h.0);
            s.0.push(i64::from(a == b));
            if s.0.len() == 2 {
                r.world_mut().despawn(a);
            }
        })
        .after(ExtractionSet::Components),
    );
    app.world_mut().init_resource::<Seen>();
    app.tick(DT).unwrap();
    let first = app.render_snapshot().entity_for(source).unwrap();
    app.tick(DT).unwrap();
    assert!(
        app.render_snapshot().entity_for(source).is_none(),
        "manually despawned render entity is not reported live"
    );
    app.tick(DT).unwrap();
    let second = app.render_snapshot().entity_for(source).unwrap();
    assert_ne!(first, second);
    assert_eq!(app.world().resource::<Seen>().0, [1, 1, 1]);
}

#[derive(Resource)]
struct Handle(Entity);

#[test]
fn custom_jobs_after_components_see_this_ticks_extraction() {
    let mut app = extraction_app();
    app.world_mut().init_resource::<Seen>();
    let e = app.world_mut().spawn(Mesh(0)).id();
    update(&mut app).add_systems(|mut q: Query<&mut Mesh>| q.iter_mut().for_each(|mut m| m.0 += 1));
    app.schedule_mut(Stage::Extract).add_systems(
        (|r: Res<RenderWorld>, mut s: ResMut<Seen>| {
            s.0.push(r.world().iter_entities().count() as i64);
        })
        .after(ExtractionSet::Components),
    );
    app.schedule_mut(Stage::Extract).add_systems(
        (|r: Res<RenderWorld>, mut s: ResMut<Seen>| {
            s.0.push(r.world().iter_entities().count() as i64 * 10);
        })
        .before(ExtractionSet::Components)
        .after(ExtractionSet::Cleanup)
        .ambiguous_with(ExtractionSet::Components),
    );
    app.tick(DT).unwrap();
    let seen = &app.world().resource::<Seen>().0;
    // Before components: only default render-world entities; after: one more.
    assert_eq!(seen[1] - seen[0] / 10, 1, "{seen:?}");
    assert_eq!(render_value::<Mesh>(&app, e), Some(Mesh(1)));
}

#[test]
fn unordered_custom_extraction_that_shares_the_render_world_is_rejected() {
    let mut app = extraction_app();
    app.schedule_mut(Stage::Extract)
        .add_systems(|_: ResMut<RenderWorld>| {});
    app.world_mut().spawn(Mesh(1));
    assert!(app.tick(DT).is_err());
    assert_eq!(app.state(), AppState::Failed);
}

#[derive(Component, Debug, Clone, PartialEq)]
struct Big(u64);

fn mirror_invariant(seed: u64, entities: usize, ticks: usize) {
    let mut rng = Rng::new(seed);
    let clones = Arc::new(AtomicUsize::new(0));
    let mut app = app();
    app.extract_component::<Mesh>();
    app.extract_component::<Big>();
    app.extract_component::<Counted>();
    let mut live: Vec<Entity> = Vec::new();
    let mut expected_clones = 0_usize;
    let mut model_mesh: HashMap<Entity, u32> = HashMap::new();
    let mut model_counted: HashMap<Entity, u32> = HashMap::new();
    for _ in 0..ticks {
        let mut dirty_counted: HashSet<Entity> = HashSet::new();
        for _ in 0..rng.below(entities as u64 / 2 + 2) {
            match rng.below(7) {
                0 | 1 if live.len() < entities => {
                    let e = app.world_mut().spawn(Mesh(1)).id();
                    live.push(e);
                    model_mesh.insert(e, 1);
                }
                2 if !live.is_empty() => {
                    let e = live.swap_remove(rng.below(live.len() as u64) as usize);
                    app.world_mut().despawn(e);
                    model_mesh.remove(&e);
                    model_counted.remove(&e);
                    dirty_counted.remove(&e);
                }
                3 if !live.is_empty() => {
                    let e = live[rng.below(live.len() as u64) as usize];
                    let v = rng.below(1000) as u32;
                    if let Some(mut m) = app.world_mut().get_mut::<Mesh>(e) {
                        m.0 = v;
                        model_mesh.insert(e, v);
                    }
                }
                4 if !live.is_empty() => {
                    let e = live[rng.below(live.len() as u64) as usize];
                    app.world_mut().entity_mut(e).remove::<Mesh>();
                    model_mesh.remove(&e);
                }
                5 if !live.is_empty() => {
                    let e = live[rng.below(live.len() as u64) as usize];
                    let v = rng.below(1000) as u32;
                    app.world_mut().entity_mut(e).insert(Counted {
                        value: v,
                        clones: Arc::clone(&clones),
                    });
                    model_counted.insert(e, v);
                    dirty_counted.insert(e);
                }
                _ if !live.is_empty() => {
                    let e = live[rng.below(live.len() as u64) as usize];
                    app.world_mut()
                        .entity_mut(e)
                        .insert(Mesh(rng.below(1000) as u32));
                    model_mesh.insert(e, app.world().get::<Mesh>(e).unwrap().0);
                }
                _ => {}
            }
        }
        expected_clones += dirty_counted.len();
        app.tick(DT).unwrap();
        // Mirror: render state equals simulation state for every live entity.
        for e in &live {
            assert_eq!(
                render_value::<Mesh>(&app, *e).map(|m| m.0),
                model_mesh.get(e).copied(),
                "seed {seed}"
            );
            let rendered = app
                .render_snapshot()
                .entity_for(*e)
                .and_then(|r| app.render_world().get::<Counted>(r).map(|c| c.value));
            assert_eq!(rendered, model_counted.get(e).copied(), "seed {seed}");
        }
        // No render entity may refer to a dead source.
        let sources: Vec<Entity> = app
            .render_world_mut()
            .query::<&ExtractedFrom>()
            .iter(app.render_world())
            .map(|f| f.0)
            .collect();
        let unique: HashSet<_> = sources.iter().copied().collect();
        assert_eq!(unique.len(), sources.len(), "one render entity per source");
        assert!(
            sources.iter().all(|s| live.contains(s)),
            "seed {seed}: stale source mapped"
        );
    }
    assert_eq!(
        clones.load(Ordering::SeqCst),
        expected_clones,
        "seed {seed}: clones track exact dirtiness"
    );
}

macro_rules! mirror_cases {
    ($($name:ident: $seed:expr, $entities:expr, $ticks:expr;)*) => {
        $( #[test] fn $name() { mirror_invariant($seed, $entities, $ticks); } )*
    };
}

mirror_cases! {
    render_mirrors_simulation_small_world: 1, 6, 60;
    render_mirrors_simulation_medium_world: 2, 30, 80;
    render_mirrors_simulation_churning_tiny_world: 3, 3, 150;
    render_mirrors_simulation_large_world: 4, 120, 50;
    render_mirrors_simulation_other_seed: 77, 20, 100;
}
