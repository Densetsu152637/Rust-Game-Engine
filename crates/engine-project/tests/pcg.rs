//! Authored `engine.PcgVolume` scene -> Runtime -> generated entities.
use engine_pcg::{Aabb, FnSurface, GraphBuilder, Registry};
use engine_project::world::coordinates::WorldOrigin;
use engine_project::world::pcg::{PcgMode, PcgNodes, PcgStatus, PcgSurfaceRegistry, PcgVolume};
use engine_project::{
    EditOperation, InputSnapshot, Project, Runtime, SceneDocument, world_transform,
};
use serde_json::{Value, json};
use std::fs;
use std::sync::Arc;

const MESH: &str = "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n";

fn graph(registry: &Registry, kind: &str) -> String {
    let entries = json!([{"asset": if kind == "prefab" {"hut"} else {"rock"}, "kind": kind}]);
    GraphBuilder::new(registry, 3)
        .node(
            "grid",
            "grid_sampler",
            [("spacing", json!(4.0)), ("jitter", json!(0.5))],
        )
        .node("spawn", "spawner", [("entries", entries)])
        .connect("grid.points", "spawn.points")
        .build()
        .unwrap()
        .to_json()
}

fn noise_graph(registry: &Registry) -> String {
    GraphBuilder::new(registry, 4)
        .node("grid", "grid_sampler", [("spacing", json!(1.0))])
        .node("n", "noise_field", [("frequency", json!(0.21))])
        .node("keep", "density_filter", [("threshold", json!(0.55))])
        .node(
            "spawn",
            "spawner",
            [("entries", json!([{"asset": "rock", "kind": "mesh"}]))],
        )
        .connect("grid.points", "keep.points")
        .connect("n.field", "keep.field")
        .connect("keep.points", "spawn.points")
        .build()
        .unwrap()
        .to_json()
}

fn fixture(volume: Value, origin_position: [f64; 3]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::builtin();
    fs::write(dir.path().join("rock.cgmesh"), MESH).unwrap();
    fs::write(
        dir.path().join("scatter.pcg.json"),
        graph(&registry, "mesh"),
    )
    .unwrap();
    fs::write(dir.path().join("huts.pcg.json"), graph(&registry, "prefab")).unwrap();
    fs::write(dir.path().join("noise.pcg.json"), noise_graph(&registry)).unwrap();
    fs::write(
        dir.path().join("hut.prefab.json"),
        json!({"schema":1,"scene_id":"hut","entities":[
            {"id":"root","name":"root","components":{"Transform":{"position":[0,0,0]}}},
            {"id":"roof","name":"roof","parent":"root","components":{"Transform":{"position":[0,2,0]},"MeshRenderer":{"mesh":"rock"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{"Transform":{"position":[0,0,50]},"Camera3D":{}}},
            {"id":"forest","name":"forest","components":{
                "Transform":{"position":origin_position},
                "custom":{"engine.PcgVolume":{"version":1,"data":volume}}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"pcg-test","startup_scene":"scene.json","assets":[
            {"id":"rock","kind":"mesh","path":"rock.cgmesh"},
            {"id":"scatter","kind":"pcg","path":"scatter.pcg.json"},
            {"id":"huts","kind":"pcg","path":"huts.pcg.json"},
            {"id":"noise","kind":"pcg","path":"noise.pcg.json"},
            {"id":"hut","kind":"prefab","path":"hut.prefab.json"}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn volume(graph: &str, seed: u64) -> Value {
    json!({"graph": graph, "seed": seed, "size": [16, 64, 16]})
}

fn start(dir: &tempfile::TempDir) -> Runtime {
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    runtime
}

fn tick(runtime: &mut Runtime) {
    runtime.tick(0.1, InputSnapshot::default()).unwrap();
}

fn generated(runtime: &Runtime) -> Vec<(String, [f32; 3])> {
    let world = runtime.app().world();
    let mut out = Vec::new();
    for n in 0..100_000 {
        let id = format!("pcg:forest:{n}");
        let Some(entity) = runtime.entity(&id) else {
            break;
        };
        out.push((id, world_transform(world, entity).unwrap().position()));
    }
    out
}

fn volume_entity(runtime: &Runtime) -> bevy_ecs::entity::Entity {
    runtime.entity("forest").unwrap()
}

#[test]
fn volume_generates_deterministic_entities_inside_its_box() {
    let dir = fixture(volume("scatter", 5), [100.0, 0.0, 0.0]);
    let mut a = start(&dir);
    tick(&mut a);
    let first = generated(&a);
    assert_eq!(first.len(), 16, "4 x 4 grid cells of 4 m over 16 m");
    for (_, p) in &first {
        assert!(
            (92.0..=108.0).contains(&p[0]) && (-8.0..=8.0).contains(&p[2]),
            "{p:?}"
        );
        assert_eq!(p[1], 0.0, "default surface is flat ground at y = 0");
    }
    // Rendered through the ordinary mesh path.
    let frame = a.extract_scene_frame().unwrap();
    assert_eq!(frame.meshes.len(), 16);
    let status = a.app().world().resource::<PcgStatus>();
    assert_eq!(status.0["forest"].instances, 16);
    assert!(status.0["forest"].diagnostics.is_empty());

    // A second run of the same project reproduces the placement exactly.
    let mut b = start(&dir);
    tick(&mut b);
    assert_eq!(generated(&b), first);
    // More ticks change nothing.
    tick(&mut a);
    tick(&mut a);
    assert_eq!(generated(&a), first);
    assert_eq!(
        a.app().world().resource::<PcgStatus>().0["forest"].attempts,
        1
    );
}

#[test]
fn changing_the_volume_or_bumping_regenerate_replaces_exactly_the_previous_output() {
    let dir = fixture(volume("scatter", 5), [0.0, 0.0, 0.0]);
    let mut runtime = start(&dir);
    tick(&mut runtime);
    let original = generated(&runtime);
    let live = runtime.app().world().iter_entities().count();
    let old_entity = runtime.entity("pcg:forest:0").unwrap();

    // New seed: same ids, different positions, same number of live entities.
    let volume = volume_entity(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .seed = 6;
    tick(&mut runtime);
    let reseeded = generated(&runtime);
    assert_eq!(reseeded.len(), 16);
    assert_ne!(reseeded, original);
    assert_eq!(
        runtime.app().world().iter_entities().count(),
        live,
        "no leaked entities"
    );
    assert!(
        runtime.app().world().get_entity(old_entity).is_err(),
        "old entity despawned"
    );

    // Back to the original seed: byte-identical to the first generation.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .seed = 5;
    tick(&mut runtime);
    assert_eq!(generated(&runtime), original);

    // An unchanged volume with a bumped counter regenerates too (attempt count grows).
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .regenerate = 1;
    tick(&mut runtime);
    assert_eq!(generated(&runtime), original);
    assert_eq!(
        runtime.app().world().resource::<PcgStatus>().0["forest"].attempts,
        4
    );

    // Baked volumes drop their runtime output and stay empty.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .mode = PcgMode::Baked;
    tick(&mut runtime);
    assert!(generated(&runtime).is_empty());
    tick(&mut runtime);
    assert!(generated(&runtime).is_empty());
}

#[test]
fn failed_generation_keeps_the_previous_output_and_reports_diagnostics() {
    let dir = fixture(volume("scatter", 5), [0.0, 0.0, 0.0]);
    let mut runtime = start(&dir);
    tick(&mut runtime);
    let original = generated(&runtime);
    let volume = volume_entity(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .surface = "terrain".into();
    tick(&mut runtime);
    assert_eq!(generated(&runtime), original);
    let report = runtime.app().world().resource::<PcgStatus>().0["forest"].clone();
    assert!(
        report.diagnostics[0].contains("pcg.surface.unknown"),
        "{report:?}"
    );
    let log = &runtime
        .app()
        .world()
        .resource::<engine_project::runtime::RuntimeLog>()
        .0;
    assert!(log.iter().any(|l| l.contains("pcg forest")));

    // The instance cap is a hard bound.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .surface = String::new();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<PcgVolume>(volume)
        .unwrap()
        .max_instances = 4;
    tick(&mut runtime);
    assert_eq!(generated(&runtime), original);
    assert!(
        runtime.app().world().resource::<PcgStatus>().0["forest"].diagnostics[0]
            .contains("pcg.limit.instances")
    );
}

#[test]
fn registered_surface_places_instances_on_the_ground() {
    let mut data = volume("scatter", 5);
    data["surface"] = json!("hills");
    let dir = fixture(data, [0.0, 0.0, 0.0]);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<PcgSurfaceRegistry>()
        .insert(
            "hills",
            Arc::new(FnSurface::new(
                Aabb::new([-1000.0, -100.0, -1000.0], [1000.0, 100.0, 1000.0]),
                |x, z| x * 0.5 + z * 0.25,
            )),
        );
    runtime.start().unwrap();
    tick(&mut runtime);
    let placed = generated(&runtime);
    assert_eq!(placed.len(), 16);
    for (_, p) in &placed {
        assert!((p[1] - (p[0] * 0.5 + p[2] * 0.25)).abs() < 1e-3, "{p:?}");
    }
}

#[test]
fn noise_follows_absolute_coordinates_when_the_origin_moves() {
    let kept = |origin_offset: [f64; 3], position: [f64; 3]| {
        let dir = fixture(volume("noise", 7), position);
        let project = Project::load(dir.path().join("project.json")).unwrap();
        let mut runtime = Runtime::new(project).unwrap();
        runtime.app_mut().world_mut().insert_resource(WorldOrigin {
            offset: origin_offset,
        });
        runtime.start().unwrap();
        tick(&mut runtime);
        let mut absolute: Vec<[i64; 2]> = generated(&runtime)
            .iter()
            .map(|(_, p)| {
                [
                    ((f64::from(p[0]) + origin_offset[0]) * 100.0).round() as i64,
                    ((f64::from(p[2]) + origin_offset[2]) * 100.0).round() as i64,
                ]
            })
            .collect();
        absolute.sort_unstable();
        absolute
    };
    let at_origin = kept([0.0; 3], [0.0; 3]);
    // The same absolute box seen from an origin that moved by (8, 0, -4).
    let rebased = kept([8.0, 0.0, -4.0], [-8.0, 0.0, 4.0]);
    assert!(
        !at_origin.is_empty() && at_origin.len() < 256,
        "{} kept",
        at_origin.len()
    );
    assert_eq!(at_origin, rebased);
    // A different absolute place yields a different pattern.
    let elsewhere = kept([4000.0, 0.0, 4000.0], [0.0; 3]);
    assert_ne!(at_origin, elsewhere);
}

#[test]
fn prefab_instances_spawn_an_anchor_with_remapped_children() {
    let dir = fixture(volume("huts", 5), [10.0, 0.0, 0.0]);
    let mut runtime = start(&dir);
    tick(&mut runtime);
    let world = runtime.app().world();
    for n in 0..16 {
        let anchor = runtime.entity(&format!("pcg:forest:{n}")).unwrap();
        let roof = runtime.entity(&format!("pcg:forest:{n}/roof")).unwrap();
        assert!(runtime.entity(&format!("pcg:forest:{n}/root")).is_some());
        let a = world_transform(world, anchor).unwrap().position();
        let r = world_transform(world, roof).unwrap().position();
        assert!((r[1] - a[1] - 2.0).abs() < 1e-4 && (r[0] - a[0]).abs() < 1e-4);
    }
    assert_eq!(runtime.extract_scene_frame().unwrap().meshes.len(), 16);
    assert_eq!(
        runtime.app().world().resource::<PcgStatus>().0["forest"].entities,
        16 * 3
    );
}

#[test]
fn custom_node_kinds_registered_on_the_world_resource_run() {
    let dir = fixture(volume("scatter", 5), [0.0; 3]);
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let runtime = Runtime::new(project).unwrap();
    let nodes = runtime.app().world().resource::<PcgNodes>();
    assert!(nodes.0.definition("poisson_sampler").is_some());
}

#[test]
fn component_set_edits_validate_and_drive_the_runtime() {
    let dir = fixture(volume("scatter", 5), [0.0; 3]);
    let mut document = SceneDocument::load(dir.path().join("project.json")).unwrap();
    let set = |data: Value, merge: bool| EditOperation::SetComponent {
        entity_id: "forest".into(),
        component: "engine.PcgVolume".into(),
        value: json!({"version": 1, "data": data}),
        merge,
    };
    let revision = document.revision().to_owned();
    for bad in [
        json!({"size": [0, 10, 10]}),
        json!({"size": [10, 10]}),
        json!({"seed": -1}),
        json!({"mode": "sometimes"}),
        json!({"max_instances": 0}),
        json!({"max_instances": 70000}),
        json!({"extra": 1}),
        json!({"graph": "does-not-exist"}),
        json!({"graph": "rock"}),
    ] {
        assert!(
            document.apply(set(bad.clone(), true), &revision).is_err(),
            "{bad} should be rejected"
        );
    }
    let revision = document
        .apply(
            set(
                json!({"graph": "scatter", "seed": 9, "size": [8, 64, 8], "regenerate": 2}),
                true,
            ),
            &revision,
        )
        .unwrap();
    document.save(&revision).unwrap();

    let project = Project::load(dir.path().join("project.json")).unwrap();
    let descriptor = &project.component_catalog.0["engine.PcgVolume"];
    assert_eq!(descriptor.requires, ["Transform"]);
    assert_eq!(descriptor.schema["additionalProperties"], false);
    assert_eq!(descriptor.defaults["max_instances"], 4096);
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    tick(&mut runtime);
    let volume = runtime
        .app()
        .world()
        .get::<PcgVolume>(volume_entity(&runtime))
        .unwrap()
        .clone();
    assert_eq!(
        (volume.seed, volume.regenerate, volume.size),
        (9, 2, [8.0, 64.0, 8.0])
    );
    assert_eq!(generated(&runtime).len(), 4, "8 m box, 4 m grid");
}

#[test]
fn malformed_pcg_assets_are_rejected_at_load() {
    let dir = fixture(volume("scatter", 5), [0.0; 3]);
    fs::write(dir.path().join("scatter.pcg.json"), r#"{"version": 99}"#).unwrap();
    let error = Project::load(dir.path().join("project.json")).unwrap_err();
    assert!(error.to_string().contains("pcg.graph.version"), "{error}");
    fs::write(dir.path().join("scatter.pcg.json"), "not json").unwrap();
    assert!(Project::load(dir.path().join("project.json")).is_err());
}

#[test]
fn a_generated_id_that_clashes_with_a_foreign_entity_fails_the_volume() {
    let dir = fixture(volume("scatter", 3), [0.0; 3]);
    let scene_path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
    scene["entities"].as_array_mut().unwrap().push(json!(
        {"id":"pcg:forest:0","name":"squatter","components":{"Transform":{"position":[9,9,9]}}}
    ));
    fs::write(&scene_path, scene.to_string()).unwrap();
    let mut runtime = start(&dir);
    tick(&mut runtime);
    let status = runtime
        .app()
        .world()
        .resource::<PcgStatus>()
        .0
        .get("forest")
        .cloned()
        .unwrap();
    assert!(
        status
            .diagnostics
            .iter()
            .any(|d| d.contains("pcg.entity.id_collision")),
        "{:?}",
        status.diagnostics
    );
    assert!(generated(&runtime).len() <= 1, "nothing was spawned");
    let squatter = runtime.entity("pcg:forest:0").unwrap();
    let position = world_transform(runtime.app().world(), squatter)
        .unwrap()
        .position();
    assert_eq!(position, [9.0, 9.0, 9.0], "the foreign entity is untouched");
}
