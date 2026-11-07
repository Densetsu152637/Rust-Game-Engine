//! The cost of entities the engine knows nothing about.
//!
//! A game keeps simulation entities that carry no engine component (a factory's idle
//! buildings, a crowd of far-away agents). The runtime must not pay for them: every
//! per-tick and per-frame pass of the engine is a narrow typed query over the components
//! it consumes, so an entity without any of them costs nothing per tick.
//!
//! `idle_entities_do_not_cost_the_runtime_tick_or_extraction` measures `Runtime::tick`
//! and `Runtime::extract_scene_frame` of a small authored scene with and without 200,000
//! such entities. Run it in release to see the figures:
//!
//! ```text
//! cargo test -p engine-project --release --test runtime_entity_walk -- --nocapture
//! ```
//!
//! In release the overhead of each is gated at 1 ms (best of many samples, so a noisy
//! machine does not fail it). A debug build (the ordinary `cargo test`) uses a tenth of
//! the entities and checks only that the scene still extracts the same frame.
//!
//! `static_transforms_cost_a_change_check_not_a_recompute` does the same with entities that
//! do carry a `Transform` but never move: world transforms are change driven, so a tick
//! checks them but recomputes and rewrites none.
use bevy_ecs::prelude::*;
use engine_project::ActionFrame;
use engine_project::{Project, Runtime, Transform, WorldTransform};
use serde_json::json;
use std::fs;
use std::time::{Duration, Instant};

const DT: f32 = 1.0 / 60.0;
/// The most the idle entities may add to one tick or one extraction, in release.
const BUDGET_MS: f64 = 1.0;

/// Stands for any game component the engine does not know.
#[derive(Component)]
struct Idle(#[allow(dead_code)] u32);

fn idle_count() -> usize {
    if cfg!(debug_assertions) {
        20_000
    } else {
        200_000
    }
}

fn samples() -> usize {
    if cfg!(debug_assertions) { 6 } else { 80 }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(dir.path().join("material.wgsl"), "custom shader source").unwrap();
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":[
            {"id":"camera","name":"Camera","components":{
                "Transform":{"position":[0,0,4]},"Camera3D":{}}},
            {"id":"sun","name":"Sun","components":{"DirectionalLight":{}}},
            {"id":"parent","name":"Parent","components":{
                "Transform":{"position":[1,0,0],"scale":[2,2,2]}}},
            {"id":"mesh","name":"Mesh","parent":"parent","components":{
                "Transform":{"position":[1,0,0]},
                "MeshRenderer":{"mesh":"mesh-asset"},
                "MeshMaterial":{"shader":"shader-asset"}}}
        ]}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"entity-walk","startup_scene":"scene.json",
            "assets":[
                {"id":"mesh-asset","kind":"mesh","path":"mesh.cgmesh"},
                {"id":"shader-asset","kind":"shader","path":"material.wgsl"}
            ]}),
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

struct Timing {
    mean: Duration,
    best: Duration,
}
impl Timing {
    fn of(mut times: Vec<Duration>) -> Self {
        times.sort();
        let total: Duration = times.iter().sum();
        Self {
            mean: total / times.len() as u32,
            best: times[0],
        }
    }
    fn show(&self) -> String {
        format!(
            "mean {:.3} ms, best {:.3} ms",
            self.mean.as_secs_f64() * 1e3,
            self.best.as_secs_f64() * 1e3
        )
    }
}
fn ms(a: Duration, b: Duration) -> f64 {
    (a.as_secs_f64() - b.as_secs_f64()) * 1e3
}

fn time_ticks(runtime: &mut Runtime, count: usize) -> Timing {
    Timing::of(
        (0..count)
            .map(|_| {
                let started = Instant::now();
                runtime.tick(DT, ActionFrame::default()).unwrap();
                started.elapsed()
            })
            .collect(),
    )
}

fn time_extraction(runtime: &Runtime, count: usize) -> Timing {
    Timing::of(
        (0..count)
            .map(|_| {
                let started = Instant::now();
                let frame = runtime.extract_scene_frame().unwrap();
                let elapsed = started.elapsed();
                assert_eq!(frame.meshes.len(), 1);
                elapsed
            })
            .collect(),
    )
}

/// Spawns `extra` through `spawn`, then reports and gates what they add to a tick and to an
/// extraction of the fixture scene.
fn measure(what: &str, spawn: impl FnOnce(&mut Runtime, usize)) {
    let dir = fixture();
    let samples = samples();
    let idle = idle_count();
    let mut runtime = runtime(&dir);
    let base_tick = time_ticks(&mut runtime, samples);
    let base_extract = time_extraction(&runtime, samples);
    let before = runtime.app().world().iter_entities().count();

    spawn(&mut runtime, idle);
    // The first ticks after the spawn flush one-time work.
    for _ in 0..3 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    let idle_tick = time_ticks(&mut runtime, samples);
    let idle_extract = time_extraction(&runtime, samples);
    let after = runtime.app().world().iter_entities().count();

    let tick_ms = ms(idle_tick.best, base_tick.best);
    let extract_ms = ms(idle_extract.best, base_extract.best);
    println!(
        "entity-walk [{what}] build: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!("entity-walk [{what}] entities: {before} -> {after} (+{idle})");
    println!("entity-walk [{what}] tick without:    {}", base_tick.show());
    println!("entity-walk [{what}] tick with:       {}", idle_tick.show());
    println!("entity-walk [{what}] tick overhead:   {tick_ms:+.3} ms (best of {samples})");
    println!(
        "entity-walk [{what}] extract without: {}",
        base_extract.show()
    );
    println!(
        "entity-walk [{what}] extract with:    {}",
        idle_extract.show()
    );
    println!("entity-walk [{what}] extract overhead:{extract_ms:+.3} ms (best of {samples})");
    assert_eq!(after - before, idle);
    if !cfg!(debug_assertions) {
        assert!(
            tick_ms <= BUDGET_MS,
            "{idle} {what} entities cost {tick_ms:.3} ms per tick (budget {BUDGET_MS} ms)"
        );
        assert!(
            extract_ms <= BUDGET_MS,
            "{idle} {what} entities cost {extract_ms:.3} ms per extraction (budget {BUDGET_MS} ms)"
        );
    }
}

#[test]
fn idle_entities_do_not_cost_the_runtime_tick_or_extraction() {
    measure("idle", |runtime, count| {
        runtime
            .app_mut()
            .world_mut()
            .spawn_batch((0..count as u32).map(Idle));
    });
}

#[test]
fn static_transforms_cost_a_change_check_not_a_recompute() {
    measure("static transform", |runtime, count| {
        runtime
            .app_mut()
            .world_mut()
            .spawn_batch((0..count as u32).map(|i| (Idle(i), Transform::default())));
        // The first refresh gives each its world transform, once.
        runtime.tick(DT, ActionFrame::default()).unwrap();
        let world = runtime.app().world();
        assert_eq!(
            world
                .try_query::<&WorldTransform>()
                .unwrap()
                .iter(world)
                .count(),
            count + 3,
            "every transformed entity has a world transform"
        );
    });
}
