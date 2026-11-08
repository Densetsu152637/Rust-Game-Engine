//! Retained mesh records: the dirty-copy rule of `Runtime::extract_scene_frame`.
//!
//! The runtime keeps one render record per mesh renderer between extractions and rebuilds, from
//! the world, only the records of entities whose render-relevant components changed since the
//! previous extraction (`runtime/retained.rs`). These tests pin the contract:
//!
//! * a static scene's second extraction copies no record, and moving one entity copies one;
//! * a despawned entity, or one that lost its `MeshRenderer`, loses its record;
//! * changes made while no frame was extracted (an occluded window) are not lost;
//! * after any sequence of edits the frame equals the one rebuilt from scratch
//!   (`extract_scene_frame_from_scratch`), with and without culling.
//!
//! `retained_extraction_reports_changed_copied_culled_and_drawn` prints the counts of a large
//! static scene; in release run it with
//!
//! ```text
//! cargo test -p engine-project --release --test runtime_retained_extraction -- --nocapture --test-threads=1
//! ```
use bevy_ecs::entity::Entity;
use engine_project::content::{Mesh, Vertex};
use engine_project::rendering3d::{MeshMaterial, SceneRenderFrame};
use engine_project::{ActionFrame, GeometryOverride, MeshRenderer, Project, Runtime, Transform};
use serde_json::json;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

const DT: f32 = 1.0 / 60.0;

fn strip(vertices: usize, scale: f32) -> Arc<Mesh> {
    Arc::new(Mesh {
        vertices: (0..vertices)
            .map(|i| Vertex {
                position: [(i / 2) as f32 * 0.01 * scale, (i % 2) as f32 * scale, 0.0],
                uv: [0.0; 2],
                color: [1.0; 4],
            })
            .collect(),
        indices: (0..(vertices as u32 - 2))
            .flat_map(|i| [i, i + 1, i + 2])
            .collect(),
        ..Mesh::default()
    })
}

/// A camera, a sun, a parent and `count` meshes: every fourth one a child of the parent.
fn project(count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    let mut entities = vec![
        json!({"id":"camera","name":"Camera","components":{
            "Transform":{"position":[0,0,6]},"Camera3D":{}}}),
        json!({"id":"sun","name":"Sun","components":{"DirectionalLight":{}}}),
        json!({"id":"parent","name":"Parent","components":{
            "Transform":{"position":[0,0,0]}}}),
    ];
    for i in 0..count {
        let mut entity = json!({"id":format!("mesh-{i}"),"name":"Mesh","components":{
            "Transform":{"position":[(i % 20) as f32 - 10.0, (i / 20) as f32, 0.0]},
            "MeshRenderer":{"mesh":"mesh-asset"},
            "MeshMaterial":{}}});
        if i % 4 == 0 {
            entity["parent"] = json!("parent");
        }
        entities.push(entity);
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"retained","startup_scene":"scene.json",
            "assets":[{"id":"mesh-asset","kind":"mesh","path":"mesh.cgmesh"}]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}

fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    for _ in 0..3 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    runtime
}

fn entity(runtime: &Runtime, id: &str) -> Entity {
    runtime.entity(id).unwrap_or_else(|| panic!("{id}"))
}

fn move_to(runtime: &mut Runtime, id: &str, position: [f32; 3]) {
    let entity = entity(runtime, id);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .position = position;
}

fn debug_of(frame: &SceneRenderFrame) -> String {
    format!("{frame:?}")
}

/// The frame of an incremental extraction equals the one rebuilt from scratch.
fn assert_matches_scratch(runtime: &Runtime, what: &str) -> SceneRenderFrame {
    let incremental = runtime.extract_scene_frame().unwrap();
    let stats = runtime.extraction_stats();
    let scratch = runtime.extract_scene_frame_from_scratch().unwrap();
    assert_eq!(
        debug_of(&incremental),
        debug_of(&scratch),
        "{what}: the incremental frame differs from the one built from scratch"
    );
    assert_eq!(stats.drawn, incremental.meshes.len());
    incremental
}

#[test]
fn a_static_scene_copies_nothing_on_its_second_extraction() {
    let dir = project(40);
    let runtime = runtime(&dir);
    let first = runtime.extract_scene_frame().unwrap();
    let stats = runtime.extraction_stats();
    assert_eq!(stats.meshes, 40);
    assert_eq!(stats.copied, 40, "the first extraction builds every record");
    assert!(runtime.extraction_delta().reset);

    let second = runtime.extract_scene_frame().unwrap();
    let stats = runtime.extraction_stats();
    assert_eq!((stats.changed, stats.copied), (0, 0));
    assert_eq!(stats.retained, 40);
    let delta = runtime.extraction_delta();
    assert!(delta.upserted.is_empty() && delta.removed.is_empty() && !delta.reset);
    assert_eq!(debug_of(&first), debug_of(&second));
}

#[test]
fn moving_one_entity_copies_exactly_one_record() {
    let dir = project(40);
    let mut runtime = runtime(&dir);
    runtime.extract_scene_frame().unwrap();
    move_to(&mut runtime, "mesh-7", [3.0, 2.0, 0.0]);
    let frame = assert_matches_scratch(&runtime, "after moving one entity");
    let moved = frame.meshes.iter().find(|m| m.id == "mesh-7").unwrap();
    assert_eq!(moved.model[3][..3], [3.0, 2.0, 0.0]);
    // The comparison above rebuilt everything; measure the incremental pass on its own.
    move_to(&mut runtime, "mesh-9", [1.0, 1.0, 0.0]);
    runtime.extract_scene_frame().unwrap();
    let stats = runtime.extraction_stats();
    assert_eq!((stats.changed, stats.copied), (1, 1));
    assert_eq!(
        runtime.extraction_delta().upserted,
        vec![entity(&runtime, "mesh-9")]
    );
}

#[test]
fn moving_a_parent_copies_its_children() {
    let dir = project(40);
    let mut runtime = runtime(&dir);
    runtime.extract_scene_frame().unwrap();
    move_to(&mut runtime, "parent", [5.0, 5.0, 0.0]);
    runtime.extract_scene_frame().unwrap();
    // mesh-0, mesh-4, ... mesh-36 are the ten children.
    assert_eq!(runtime.extraction_stats().copied, 10);
    assert_matches_scratch(&runtime, "after moving the parent");
}

#[test]
fn despawned_entities_and_removed_components_drop_their_records() {
    let dir = project(40);
    let mut runtime = runtime(&dir);
    runtime.extract_scene_frame().unwrap();
    let gone = entity(&runtime, "mesh-3");
    let stripped = entity(&runtime, "mesh-5");
    let world = runtime.app_mut().world_mut();
    world.despawn(gone);
    world.entity_mut(stripped).remove::<MeshRenderer>();
    let frame = runtime.extract_scene_frame().unwrap();
    let stats = runtime.extraction_stats();
    assert_eq!((stats.meshes, stats.retained, stats.copied), (38, 38, 0));
    let mut removed = runtime.extraction_delta().removed;
    removed.sort();
    let mut expected = vec![gone, stripped];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(
        frame
            .meshes
            .iter()
            .all(|m| m.id != "mesh-3" && m.id != "mesh-5")
    );
    assert_matches_scratch(&runtime, "after despawn and removal");
}

#[test]
fn changes_made_while_no_frame_was_extracted_are_not_lost() {
    // An occluded window skips presentation for a while: the ticks still run, edits still
    // happen, and the next extraction that does run sees all of them.
    let dir = project(40);
    let mut runtime = runtime(&dir);
    runtime.extract_scene_frame().unwrap();
    move_to(&mut runtime, "mesh-1", [2.0, 0.0, 0.0]);
    for _ in 0..5 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    move_to(&mut runtime, "mesh-2", [4.0, 0.0, 0.0]);
    for _ in 0..5 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    let frame = runtime.extract_scene_frame().unwrap();
    assert_eq!(runtime.extraction_stats().copied, 2);
    for (id, x) in [("mesh-1", 2.0), ("mesh-2", 4.0)] {
        let draw = frame.meshes.iter().find(|m| m.id == id).unwrap();
        assert_eq!(draw.model[3][0], x, "{id}");
    }
}

#[test]
fn a_failed_extraction_is_looked_at_again() {
    let dir = project(10);
    let mut runtime = runtime(&dir);
    runtime.extract_scene_frame().unwrap();
    let broken = entity(&runtime, "mesh-2");
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MeshMaterial>(broken)
        .unwrap()
        .metallic = 7.0;
    assert!(runtime.extract_scene_frame().is_err());
    assert!(runtime.extract_scene_frame().is_err(), "still broken");
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<MeshMaterial>(broken)
        .unwrap()
        .metallic = 0.5;
    runtime.extract_scene_frame().unwrap();
    assert_matches_scratch(&runtime, "after the repair");
}

/// A small deterministic generator, so a failing sequence can be replayed from its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 2000) as f32 / 100.0 - 10.0
    }
}

#[test]
fn any_sequence_of_edits_gives_the_frame_built_from_scratch() {
    for culling in [true, false] {
        for seed in 1..=6u64 {
            let dir = project(24);
            let mut runtime = runtime(&dir);
            runtime.set_frustum_culling(culling);
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let mut spawned = 0;
            for step in 0..60 {
                for _ in 0..1 + rng.below(3) {
                    let id = format!("mesh-{}", rng.below(24));
                    let world_entity = runtime.entity(&id);
                    match rng.below(12) {
                        0 | 1 => {
                            let position = [rng.unit(), rng.unit(), rng.unit()];
                            if world_entity.is_some() {
                                move_to(&mut runtime, &id, position);
                            }
                        }
                        2 => {
                            if let Some(e) = world_entity
                                && let Some(mut m) =
                                    runtime.app_mut().world_mut().get_mut::<MeshMaterial>(e)
                            {
                                m.tint = [0.1 * rng.below(10) as f32, 0.5, 0.5, 1.0];
                            }
                        }
                        3 => {
                            if let Some(e) = world_entity {
                                let mesh = strip(4 + rng.below(30), 1.0 + rng.below(3) as f32);
                                runtime
                                    .app_mut()
                                    .world_mut()
                                    .entity_mut(e)
                                    .insert(GeometryOverride(mesh));
                            }
                        }
                        4 => {
                            if let Some(e) = world_entity {
                                runtime
                                    .app_mut()
                                    .world_mut()
                                    .entity_mut(e)
                                    .remove::<GeometryOverride>();
                            }
                        }
                        5 => {
                            if let Some(e) = world_entity {
                                runtime
                                    .app_mut()
                                    .world_mut()
                                    .entity_mut(e)
                                    .remove::<MeshMaterial>();
                            }
                        }
                        6 => {
                            if let Some(e) = world_entity {
                                runtime.app_mut().world_mut().despawn(e);
                            }
                        }
                        7 => {
                            spawned += 1;
                            let position = [rng.unit(), rng.unit(), 0.0];
                            runtime.app_mut().world_mut().spawn((
                                Transform {
                                    position,
                                    ..Transform::default()
                                },
                                MeshRenderer {
                                    version: 1,
                                    mesh: "mesh-asset".into(),
                                    texture: None,
                                },
                            ));
                        }
                        8 => {
                            if let Some(e) = world_entity {
                                runtime
                                    .app_mut()
                                    .world_mut()
                                    .entity_mut(e)
                                    .remove::<MeshRenderer>();
                            }
                        }
                        9 => move_to(&mut runtime, "parent", [rng.unit(), rng.unit(), 0.0]),
                        10 => move_to(&mut runtime, "camera", [rng.unit(), rng.unit(), 6.0]),
                        _ => runtime.tick(DT, ActionFrame::default()).unwrap(),
                    }
                }
                // Some frames are skipped (an occluded window); the changes pile up.
                if rng.below(4) != 0 {
                    assert_matches_scratch(
                        &runtime,
                        &format!("culling {culling}, seed {seed}, step {step} ({spawned} spawned)"),
                    );
                }
            }
        }
    }
}

#[test]
fn retained_extraction_reports_changed_copied_culled_and_drawn() {
    // A large static scene: half of the meshes are in front of the camera, half far to the side.
    let count = if cfg!(debug_assertions) { 400 } else { 5000 };
    let dir = project(0);
    let mut runtime = runtime(&dir);
    for i in 0..count {
        let x = if i % 2 == 0 {
            (i % 20) as f32 - 10.0
        } else {
            9000.0 + i as f32
        };
        runtime.app_mut().world_mut().spawn((
            Transform {
                position: [x, 0.0, 0.0],
                ..Transform::default()
            },
            MeshRenderer {
                version: 1,
                mesh: "mesh-asset".into(),
                texture: None,
            },
            GeometryOverride(strip(200, 1.0)),
        ));
    }
    runtime.tick(DT, ActionFrame::default()).unwrap();
    let time = |runtime: &Runtime| {
        (0..if cfg!(debug_assertions) { 3 } else { 40 })
            .map(|_| {
                let started = Instant::now();
                runtime.extract_scene_frame().unwrap();
                started.elapsed().as_secs_f64() * 1e3
            })
            .fold(f64::MAX, f64::min)
    };
    let started = Instant::now();
    runtime.extract_scene_frame().unwrap();
    let first_ms = started.elapsed().as_secs_f64() * 1e3;
    let first = runtime.extraction_stats();
    let still_ms = time(&runtime);
    let still = runtime.extraction_stats();
    move_to(&mut runtime, "camera", [1.0, 0.0, 6.0]);
    let scratch_ms = (0..if cfg!(debug_assertions) { 3 } else { 10 })
        .map(|_| {
            let started = Instant::now();
            runtime.extract_scene_frame_from_scratch().unwrap();
            started.elapsed().as_secs_f64() * 1e3
        })
        .fold(f64::MAX, f64::min);
    println!("retained-extraction {count} meshes of 200 vertices, half outside the view:");
    println!("retained-extraction first extraction ({first_ms:.3} ms): {first:?}");
    println!("retained-extraction static scene, best {still_ms:.3} ms: {still:?}");
    println!("retained-extraction rebuilt from scratch, best {scratch_ms:.3} ms");
    assert_eq!((first.copied, first.drawn), (count, count / 2));
    assert_eq!(
        (
            still.changed,
            still.copied,
            still.outside_frustum,
            still.drawn
        ),
        (0, 0, count / 2, count / 2)
    );
    if !cfg!(debug_assertions) {
        assert!(
            still_ms * 3.0 < scratch_ms,
            "a static scene ({still_ms:.3} ms) must cost far less than a rebuild ({scratch_ms:.3} ms)"
        );
    }
}
