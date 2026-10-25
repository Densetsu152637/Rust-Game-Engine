//! Authored `engine.Terrain` scene -> Runtime -> render chunks, colliders, surfaces, navigation.
use engine_pcg::{GraphBuilder, Registry};
use engine_project::simulation::{BakeSettings, NavPath, QuerySettings};
use engine_project::world::pcg::{PcgSurfaceRegistry, PcgVolume};
use engine_project::world::terrain::{
    self, TerrainComponent, TerrainGpuRenderer, TerrainRenderMode, TerrainRenderSet,
};
use engine_project::{InputSnapshot, Project, Runtime, world_transform};
use engine_terrain::noise::fbm;
use engine_terrain::{Brush, Heightfield, SculptOp, TerrainLayer, asset};
use glam::Vec3;
use serde_json::{Value, json};
use std::fs;

const MESH: &str = "CGMESH 1\nvertex -1 -1 0 0 0\nvertex 1 -1 0 1 0\nvertex 0 1 0 0 1\n";

/// 257 x 257 hills (spacing 1) with a flat plateau at height 3 around local (64, 64) and a
/// hole around local (184, 44).
fn land() -> Heightfield {
    let side = 257u32;
    let heights = (0..side * side)
        .map(|i| {
            let (c, r) = ((i % side) as f32, (i / side) as f32);
            let plateau = ((c - 64.0).powi(2) + (r - 64.0).powi(2)).sqrt();
            if plateau < 12.0 {
                3.0
            } else {
                3.0 + 6.0 * fbm(c * 0.02, r * 0.02, 3)
            }
        })
        .collect();
    let mut field = Heightfield::from_heights(side, side, 1.0, heights).unwrap();
    field
        .add_layer(TerrainLayer::new("rock", [0.5, 0.5, 0.5, 1.0]))
        .unwrap();
    for r in 40..48 {
        for c in 180..188 {
            field.set_hole(c, r, true).unwrap();
        }
    }
    field
}

fn entity(id: &str, position: [f32; 3], components: Value) -> Value {
    let mut components = components;
    components["Transform"] = json!({ "position": position });
    json!({"id": id, "name": id, "components": components})
}
fn custom(name: &str, data: Value) -> Value {
    json!({"custom": {name: {"version": 1, "data": data}}})
}

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(terrain: Value, extra: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("land.terrain"), asset::encode(&land())).unwrap();
        fs::write(dir.path().join("rock.cgmesh"), MESH).unwrap();
        let registry = Registry::builtin();
        let graph = GraphBuilder::new(&registry, 3)
            .node("grid", "grid_sampler", [("spacing", json!(4.0))])
            .node(
                "spawn",
                "spawner",
                [("entries", json!([{"asset": "rock", "kind": "mesh"}]))],
            )
            .connect("grid.points", "spawn.points")
            .build()
            .unwrap()
            .to_json();
        fs::write(dir.path().join("scatter.pcg.json"), graph).unwrap();
        let mut entities = vec![
            entity(
                "camera",
                [-10.0 + 20.0, 30.0, -10.0 + 20.0],
                json!({"Camera3D": {}}),
            ),
            entity(
                "land",
                [-10.0, 1.0, -10.0],
                custom("engine.Terrain", terrain),
            ),
        ];
        entities.extend(extra);
        fs::write(
            dir.path().join("scene.json"),
            json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("project.json"),
            json!({"schema":1,"name":"terrain-test","startup_scene":"scene.json","assets":[
                {"id":"land","kind":"terrain","path":"land.terrain"},
                {"id":"rock","kind":"mesh","path":"rock.cgmesh"},
                {"id":"scatter","kind":"pcg","path":"scatter.pcg.json"}
            ]})
            .to_string(),
        )
        .unwrap();
        Self { dir }
    }
    fn standard() -> Self {
        Self::new(
            json!({"asset":"land","lod":{"node_resolution":16},"render_mode":"cpu"}),
            vec![],
        )
    }
    fn project(&self) -> Project {
        Project::load(self.dir.path().join("project.json")).unwrap()
    }
    fn runtime(&self) -> Runtime {
        let mut runtime = Runtime::new(self.project()).unwrap();
        runtime.start().unwrap();
        runtime
    }
}

fn tick(runtime: &mut Runtime, n: usize) {
    for _ in 0..n {
        runtime.tick(0.02, InputSnapshot::default()).unwrap();
    }
}
fn chunk_meshes(runtime: &Runtime) -> Vec<engine_project::rendering3d::MeshDraw3D> {
    runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .into_iter()
        .filter(|m| m.id.starts_with("terrain:land/chunk/"))
        .collect()
}
fn move_camera(runtime: &mut Runtime, position: [f32; 3]) {
    let camera = runtime.entity("camera").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::Transform>(camera)
        .unwrap()
        .position = position;
}
fn position(runtime: &Runtime, id: &str) -> Vec3 {
    let entity = runtime.entity(id).unwrap();
    Vec3::from(
        world_transform(runtime.app().world(), entity)
            .unwrap()
            .position(),
    )
}

#[test]
fn project_validates_terrain_assets_and_the_component() {
    let fixture = Fixture::standard();
    fixture.project().validate().unwrap();
    let bad = Fixture::new(json!({"asset":"land","lod":{"lod_distance":2.0}}), vec![]);
    assert!(Project::load(bad.dir.path().join("project.json")).is_err());
    let missing = Fixture::new(json!({"asset":"nope"}), vec![]);
    let error = Project::load(missing.dir.path().join("project.json"))
        .and_then(Runtime::new)
        .err()
        .expect("unknown terrain asset");
    assert!(error.to_string().contains("nope"), "{error}");
    // A corrupt terrain file is rejected by project validation.
    let corrupt = Fixture::standard();
    let path = corrupt.dir.path().join("land.terrain");
    let mut bytes = fs::read(&path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 1;
    fs::write(&path, bytes).unwrap();
    let error = Project::load(path.with_file_name("project.json"))
        .and_then(|p| p.validate())
        .unwrap_err()
        .to_string();
    assert!(error.contains("terrain.asset.checksum"), "{error}");
}

#[test]
fn cpu_chunk_meshes_follow_the_camera_lod() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1);
    let entity = runtime.entity("land").unwrap();
    let near = terrain::status(runtime.app().world(), entity).unwrap();
    assert_eq!(near.render_path, "cpu-mesh-chunks");
    assert_eq!(near.levels, 5);
    let meshes = chunk_meshes(&runtime);
    assert_eq!(meshes.len(), near.chunk_entities);
    assert_eq!(meshes.len(), near.nodes);
    assert!(near.level_histogram[0] > 0, "{near:?}");
    // The camera is over local (20, 29, 20): the level-0 chunk there exists and is full detail.
    assert!(
        meshes.iter().any(|m| m.id == "terrain:land/chunk/0/1,1"),
        "{:?}",
        meshes.iter().map(|m| &m.id).collect::<Vec<_>>()
    );
    // Chunk geometry is in terrain-local space under the terrain's transform.
    let first = &meshes[0];
    assert_eq!(first.model[3][..3], [-10.0, 1.0, -10.0]);
    let vertex_count: usize = meshes.iter().map(|m| m.mesh.vertices.len()).sum();
    assert_eq!(vertex_count, near.vertices);
    assert_eq!(vertex_count, near.nodes * 17 * 17);
    // High above the far corner the selection is coarser and has no level-0 nodes near the
    // old position.
    move_camera(&mut runtime, [400.0, 400.0, 400.0]);
    tick(&mut runtime, 1);
    let far = terrain::status(runtime.app().world(), entity).unwrap();
    assert!(far.nodes < near.nodes, "{} vs {}", far.nodes, near.nodes);
    assert_eq!(far.level_histogram[0], 0, "{far:?}");
    let meshes = chunk_meshes(&runtime);
    assert_eq!(meshes.len(), far.nodes);
    assert!(
        !meshes
            .iter()
            .any(|m| m.id.starts_with("terrain:land/chunk/0/"))
    );
    // Back near a different corner: level-0 chunks appear there.
    move_camera(&mut runtime, [-10.0 + 240.0, 20.0, -10.0 + 240.0]);
    tick(&mut runtime, 1);
    let meshes = chunk_meshes(&runtime);
    assert!(meshes.iter().any(|m| m.id == "terrain:land/chunk/0/15,15"));
    // Vertex colours carry the splat result (base layer white everywhere).
    assert!(
        meshes[0].mesh.vertices[0]
            .color
            .iter()
            .all(|&c| (c - 1.0).abs() < 1e-6)
    );
}

#[test]
fn a_gpu_renderer_takes_over_and_cpu_chunks_stop_drawing() {
    let fixture = Fixture::new(json!({"asset":"land","lod":{"node_resolution":16}}), vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1);
    assert!(
        !chunk_meshes(&runtime).is_empty(),
        "auto without a GPU renderer: CPU"
    );
    assert!(
        runtime
            .app()
            .world()
            .resource::<TerrainRenderSet>()
            .items
            .is_empty()
    );
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(TerrainGpuRenderer);
    tick(&mut runtime, 1);
    assert!(chunk_meshes(&runtime).is_empty(), "no double draw");
    let entity = runtime.entity("land").unwrap();
    let world = runtime.app().world();
    let set = world.resource::<TerrainRenderSet>();
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "land");
    assert_eq!(set.items[0].layers.len(), 2);
    assert_eq!(set.items[0].world_from_local[3][..3], [-10.0, 1.0, -10.0]);
    assert_eq!(
        terrain::status(world, entity).unwrap().render_path,
        "gpu-feature"
    );
    // An explicit cpu render mode keeps the fallback even with the renderer installed.
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<TerrainComponent>(entity)
        .unwrap()
        .render_mode = TerrainRenderMode::Cpu;
    tick(&mut runtime, 1);
    assert!(!chunk_meshes(&runtime).is_empty());
    assert!(
        runtime
            .app()
            .world()
            .resource::<TerrainRenderSet>()
            .items
            .is_empty()
    );
}

#[test]
fn dynamic_balls_rest_on_the_terrain_and_fall_through_holes() {
    let ball = |id: &str, local: [f32; 2]| {
        entity(
            id,
            [-10.0 + local[0], 12.0, -10.0 + local[1]],
            custom(
                "engine.PhysicsBody3D",
                json!({"shape":"ball","radius":0.5,"material":{"friction":1.0}}),
            ),
        )
    };
    let fixture = Fixture::new(
        json!({"asset":"land","lod":{"node_resolution":16},"render_mode":"cpu",
            "collision":{"tile_cells":64}}),
        vec![
            ball("on_plateau", [64.0, 64.0]),
            ball("over_hole", [184.0, 44.0]),
        ],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 150);
    let entity = runtime.entity("land").unwrap();
    let status = terrain::status(runtime.app().world(), entity).unwrap();
    // 4 x 4 tiles of 64 cells; the tile with the hole is a triangle mesh.
    assert_eq!(status.heightfield_colliders, 15);
    assert_eq!(status.mesh_colliders, 1);
    // Plateau height 3 + terrain y 1 + radius 0.5; tolerance 0.05 m.
    let rest = position(&runtime, "on_plateau");
    assert!((rest.y - 4.5).abs() < 0.05, "rest {rest}");
    assert!((rest.x - 54.0).abs() < 0.05 && (rest.z - 54.0).abs() < 0.05);
    let fallen = position(&runtime, "over_hole");
    assert!(fallen.y < -5.0, "the hole lets the ball through: {fallen}");
}

#[test]
fn edits_rebuild_chunks_and_colliders() {
    let fixture = Fixture::new(
        json!({"asset":"land","lod":{"node_resolution":16},"render_mode":"cpu",
            "collision":{"tile_cells":64}}),
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1);
    let entity = runtime.entity("land").unwrap();
    let before = terrain::status(runtime.app().world(), entity).unwrap();
    let world = runtime.app_mut().world_mut();
    let report = terrain::sculpt(
        world,
        entity,
        &Brush::new([20.0, 20.0], 4.0),
        SculptOp::SetHeight { height: 5.0 },
    )
    .unwrap();
    assert!(report.changed > 0);
    let hole = terrain::paint_holes(world, entity, &Brush::new([31.0, 21.0], 0.75), true).unwrap();
    assert_eq!(hole.changed, 4);
    assert_eq!(
        terrain::height_at_world(world, entity, 10.0, 10.0).unwrap(),
        Some(6.0)
    );
    assert_eq!(
        terrain::height_at_world(world, entity, 21.0, 11.0).unwrap(),
        None,
        "over the hole"
    );
    tick(&mut runtime, 1);
    let after = terrain::status(runtime.app().world(), entity).unwrap();
    assert_eq!(after.revision, before.revision + 2);
    assert_eq!(after.mesh_colliders, before.mesh_colliders + 1);
    assert_eq!(
        after.triangles + 8,
        before.triangles,
        "4 hole cells, 2 triangles each"
    );
    // The level-0 chunk under the edit carries the new height at local (20, 20).
    let chunk = chunk_meshes(&runtime)
        .into_iter()
        .find(|m| m.id == "terrain:land/chunk/0/1,1")
        .unwrap();
    let vertex = chunk
        .mesh
        .vertices
        .iter()
        .find(|v| v.position[0] == 20.0 && v.position[2] == 20.0)
        .unwrap();
    assert_eq!(vertex.position[1], 5.0);
    // Export keeps the edits.
    let bytes = terrain::export_asset(runtime.app_mut().world_mut(), entity).unwrap();
    let saved = asset::decode(&bytes).unwrap();
    assert_eq!(saved.height(20, 20), 5.0);
    assert!(saved.is_hole(30, 20));
}

#[test]
fn the_terrain_is_a_pcg_surface_and_volumes_regenerate_on_it() {
    let fixture = Fixture::new(
        json!({"asset":"land","lod":{"node_resolution":16},"render_mode":"cpu"}),
        vec![entity(
            "forest",
            [54.0, 0.0, 54.0],
            custom(
                "engine.PcgVolume",
                json!({"graph":"scatter","size":[16,64,16],"surface":"terrain"}),
            ),
        )],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("land").unwrap();
    let world = runtime.app_mut().world_mut();
    let surface = world
        .resource::<PcgSurfaceRegistry>()
        .get("terrain")
        .unwrap();
    for (x, z) in [(0.0, 0.0), (37.3, 81.9), (120.0, 5.5)] {
        let expected = terrain::height_at_world(world, entity, x, z)
            .unwrap()
            .unwrap();
        assert!((surface.height(x, z) - expected).abs() < 1e-4);
    }
    assert_eq!(surface.layer_weight("rock", 50.0, 50.0), Some(0.0));
    assert_eq!(surface.layer_weight("base", 50.0, 50.0), Some(1.0));
    assert!(surface.normal(54.0, 54.0).abs_diff_eq(Vec3::Y, 1e-5));
    // Instances stand on the plateau (world height 4) instead of flat ground at 0.
    let mut found = 0;
    for n in 0..64 {
        let Some(instance) = runtime.entity(&format!("pcg:forest:{n}")) else {
            break;
        };
        let p = world_transform(runtime.app().world(), instance)
            .unwrap()
            .position();
        assert!((p[1] - 4.0).abs() < 1e-3, "instance {n} at {p:?}");
        found += 1;
    }
    assert!(found >= 4, "{found} instances");
    // Editing the terrain re-registers the surface and regenerates the volume.
    let forest = runtime.entity("forest").unwrap();
    let before = runtime
        .app()
        .world()
        .get::<PcgVolume>(forest)
        .unwrap()
        .regenerate;
    let world = runtime.app_mut().world_mut();
    terrain::sculpt(
        world,
        entity,
        &Brush::new([64.0, 64.0], 30.0),
        SculptOp::SetHeight { height: 5.0 },
    )
    .unwrap();
    tick(&mut runtime, 2);
    let after = runtime
        .app()
        .world()
        .get::<PcgVolume>(forest)
        .unwrap()
        .regenerate;
    assert_eq!(after, before + 1);
    let instance = runtime.entity("pcg:forest:0").unwrap();
    let p = world_transform(runtime.app().world(), instance)
        .unwrap()
        .position();
    assert!((p[1] - 6.0).abs() < 1e-3, "{p:?}");
}

#[test]
fn terrain_feeds_navmesh_baking_with_holes_and_slopes_blocking() {
    // 65 x 33 gentle slope (h = 0.05 x) at spacing 1, placed at the world origin.
    let field = |edit: &dyn Fn(&mut Heightfield)| {
        let heights = (0..65 * 33).map(|i| 0.05 * (i % 65) as f32).collect();
        let mut field = Heightfield::from_heights(65, 33, 1.0, heights).unwrap();
        edit(&mut field);
        field
    };
    let settings = BakeSettings {
        cell_size: 0.5,
        agent_radius: 0.25,
        ..BakeSettings::default()
    };
    let options = engine_terrain::NavInputOptions::default();
    let query = QuerySettings::default();
    let route = |field: &Heightfield| {
        let document =
            terrain::navigation_document(field, &options, settings.clone(), Vec::new()).unwrap();
        let mesh = document.bake().unwrap();
        let start = [4.0, field.height_at(4.0, 16.0), 16.0];
        let goal = [60.0, field.height_at(60.0, 16.0), 16.0];
        mesh.find_path(start, goal, query).unwrap()
    };
    let open = field(&|_| {});
    match route(&open) {
        NavPath::Reached { points, .. } => {
            let last = points.last().unwrap();
            assert!((last[0] - 60.0).abs() < 0.3 && (last[1] - 3.0).abs() < 0.05);
        }
        other => panic!("open terrain must be traversable: {other:?}"),
    }
    // A hole strip across the whole width blocks the route.
    let holed = field(&|f| {
        for r in 0..32 {
            f.set_hole(30, r, true).unwrap();
        }
    });
    assert_eq!(route(&holed), NavPath::Unreachable);
    // So does a steep ridge (a 6 m step over one sample, about 80 degrees).
    let ridge = field(&|f| {
        for r in 0..33 {
            let h = f.height(40, r);
            f.set_height(40, r, h + 6.0).unwrap();
        }
    });
    assert_eq!(route(&ridge), NavPath::Unreachable);
    // The runtime entity API produces the same input at the entity's translation.
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1);
    let entity = runtime.entity("land").unwrap();
    let region = Some([[0.0, 0.0], [32.0, 32.0]]);
    let input = terrain::navigation_input(
        runtime.app_mut().world_mut(),
        entity,
        BakeSettings {
            cell_size: 1.0,
            ..BakeSettings::default()
        },
        region,
    )
    .unwrap();
    assert_eq!(input.triangles.len(), 2 * 32 * 32);
    assert_eq!(input.vertices[0][0], 0.0);
}
