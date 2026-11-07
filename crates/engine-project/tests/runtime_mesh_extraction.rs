//! The cost of extracting meshes drawn by a 3D camera.
//!
//! A game attaches runtime-built geometry to entities as `GeometryOverride` (an
//! `Arc<Mesh>`) beside a `MeshRenderer` and a `MeshMaterial`. Under a `Camera3D` the 3D
//! pass draws such an entity by handle: the frame carries the `Arc<Mesh>` and the entity's
//! world matrix, and the renderer's mesh cache (keyed by the `Arc`) uploads it once. The
//! extraction must therefore cost the number of drawn entities, never the number of
//! vertices of their meshes: it must not clone, transform, serialize or hash vertex data.
//!
//! `extraction_of_3d_meshes_does_not_follow_vertex_count` extracts the same scene of 100
//! meshes with 200 and with 20,000 vertices each and reports both. Run it in release to see
//! the figures:
//!
//! ```text
//! cargo test -p engine-project --release --test runtime_mesh_extraction -- --nocapture --test-threads=1
//! ```
//!
//! In release each extraction is gated at 2 ms (best of many samples, so a noisy machine
//! does not fail it) and the large-mesh extraction at 1 ms more than the small-mesh one. A
//! debug build (the ordinary `cargo test`) gates no time; it checks the extracted frame.
//!
//! `meshes_of_a_2d_camera_still_draw_through_the_2d_path` keeps the other half of the
//! contract: without a 3D camera a `MeshRenderer` is a 2D draw, baked in world space.
use engine_project::content::{Mesh, Vertex};
use engine_project::{ActionFrame, GeometryOverride, Project, Runtime};
use serde_json::json;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DT: f32 = 1.0 / 60.0;
const MESHES: usize = 100;
const SMALL_VERTICES: usize = 200;
const LARGE_VERTICES: usize = 20_000;
/// The most one extraction of the scene may take, in release.
const BUDGET_MS: f64 = 2.0;
/// The most the large meshes may add to an extraction of the small ones, in release.
const VERTEX_BUDGET_MS: f64 = 1.0;

fn samples() -> usize {
    if cfg!(debug_assertions) { 3 } else { 60 }
}

/// A triangle strip of `vertices` vertices on a patch of the XY plane.
fn strip(vertices: usize) -> Mesh {
    let vertices_of = (0..vertices).map(|i| {
        let x = (i / 2) as f32 * 0.001;
        let y = (i % 2) as f32;
        Vertex {
            position: [x, y, 0.0],
            uv: [x, y],
            color: [1.0; 4],
        }
    });
    Mesh {
        vertices: vertices_of.collect(),
        indices: (0..(vertices as u32 - 2))
            .flat_map(|i| [i, i + 1, i + 2])
            .collect(),
        ..Mesh::default()
    }
}

fn project(camera: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mesh.cgmesh"),
        "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(dir.path().join("material.wgsl"), "custom shader source").unwrap();
    let mut entities = vec![
        json!({"id":"camera","name":"Camera","components":camera}),
        json!({"id":"sun","name":"Sun","components":{"DirectionalLight":{}}}),
    ];
    for i in 0..MESHES {
        entities.push(json!({"id":format!("mesh-{i}"),"name":"Mesh","components":{
            "Transform":{"position":[i as f32, 0, 0]},
            "MeshRenderer":{"mesh":"mesh-asset"},
            "MeshMaterial":{"shader":"shader-asset"}}}));
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"scene","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"mesh-extraction","startup_scene":"scene.json",
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

/// A started runtime whose 100 mesh entities hold `vertices`-vertex `GeometryOverride`s.
fn runtime(dir: &tempfile::TempDir, vertices: usize) -> (Runtime, Arc<Mesh>) {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    let geometry = Arc::new(strip(vertices));
    let world = runtime.app_mut().world_mut();
    let entities: Vec<_> = world
        .query::<(bevy_ecs::entity::Entity, &engine_project::MeshRenderer)>()
        .iter(world)
        .map(|(entity, _)| entity)
        .collect();
    assert_eq!(entities.len(), MESHES);
    for entity in entities {
        world
            .entity_mut(entity)
            .insert(GeometryOverride(geometry.clone()));
    }
    for _ in 0..3 {
        runtime.tick(DT, ActionFrame::default()).unwrap();
    }
    (runtime, geometry)
}

fn best_of(
    runtime: &Runtime,
    count: usize,
    check: impl Fn(&engine_project::rendering3d::SceneRenderFrame),
) -> Duration {
    (0..count)
        .map(|_| {
            let started = Instant::now();
            let frame = runtime.extract_scene_frame().unwrap();
            let elapsed = started.elapsed();
            check(&frame);
            elapsed
        })
        .min()
        .unwrap()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

#[test]
fn extraction_of_3d_meshes_does_not_follow_vertex_count() {
    let dir = project(json!({"Transform":{"position":[0,0,4]},"Camera3D":{}}));
    let mut results = Vec::new();
    for vertices in [SMALL_VERTICES, LARGE_VERTICES] {
        let (runtime, geometry) = runtime(&dir, vertices);
        let check = |frame: &engine_project::rendering3d::SceneRenderFrame| {
            assert_eq!(frame.meshes.len(), MESHES, "every mesh draws in 3D");
            assert!(
                frame.sprites.draws.is_empty(),
                "meshes drawn in 3D are not extracted a second time for the 2D layer"
            );
            assert!(
                frame
                    .meshes
                    .iter()
                    .all(|draw| Arc::ptr_eq(&draw.mesh, &geometry)),
                "the frame carries the mesh by handle, not a copy"
            );
            assert!(frame.materials2d.is_empty());
        };
        // The first extraction reads the material's shader once.
        runtime.extract_scene_frame().unwrap();
        let best = best_of(&runtime, samples(), check);
        println!(
            "mesh-extraction build: {}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        println!(
            "mesh-extraction {MESHES} meshes x {vertices} vertices: best {:.3} ms of {}",
            ms(best),
            samples()
        );
        results.push(best);
    }
    let (small, large) = (results[0], results[1]);
    println!(
        "mesh-extraction vertices x{}: {:+.3} ms",
        LARGE_VERTICES / SMALL_VERTICES,
        ms(large) - ms(small)
    );
    if !cfg!(debug_assertions) {
        assert!(
            ms(large) <= BUDGET_MS,
            "extracting {MESHES} meshes of {LARGE_VERTICES} vertices took {:.3} ms (budget {BUDGET_MS} ms)",
            ms(large)
        );
        assert!(
            ms(large) - ms(small) <= VERTEX_BUDGET_MS,
            "{LARGE_VERTICES}-vertex meshes cost {:.3} ms more than {SMALL_VERTICES}-vertex ones (budget {VERTEX_BUDGET_MS} ms)",
            ms(large) - ms(small)
        );
    }
}

#[test]
fn meshes_of_a_2d_camera_still_draw_through_the_2d_path() {
    let dir = project(json!({"Transform":{"position":[0,0,0]},"Camera2D":{}}));
    let (runtime, geometry) = runtime(&dir, SMALL_VERTICES);
    let frame = runtime.extract_scene_frame().unwrap();
    assert!(frame.camera3d.is_none());
    assert!(frame.meshes.is_empty());
    let draws: Vec<_> = frame
        .sprites
        .draws
        .iter()
        .filter(|draw| draw.id.starts_with("mesh:"))
        .collect();
    assert_eq!(draws.len(), MESHES);
    // Baked into world space: entity `i` stands at x = i.
    let draw = draws
        .iter()
        .find(|draw| draw.id == "mesh:mesh-7")
        .expect("mesh-7 draws");
    assert_eq!(draw.mesh.vertices.len(), SMALL_VERTICES);
    assert_eq!(
        draw.mesh.vertices[0].position[0],
        geometry.vertices[0].position[0] + 7.0
    );
    assert!(draw.state.depth_test && draw.state.depth_write);
}
