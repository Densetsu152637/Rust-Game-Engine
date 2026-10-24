//! Authored tilemap scene -> Runtime -> render geometry and collision.
use engine_project::runtime::{BlendMode, DrawSpace};
use engine_project::world::tilemap::{self, TilemapComponent};
use engine_project::{InputSnapshot, Project, Runtime};
use engine_tilemap::{CellRect, CollisionKind, RuleTile, TileDef, Tilemap, TilesetDocument};
use serde_json::{Value, json};
use std::fs;

fn tile(id: u32, source: [u32; 4], collision: CollisionKind) -> TileDef {
    TileDef {
        id,
        name: format!("tile{id}"),
        source,
        collision,
        properties: [(
            "kind".to_owned(),
            engine_tilemap::PropertyValue::Text("test".into()),
        )]
        .into_iter()
        .collect(),
    }
}
fn tileset_json() -> String {
    let mut doc = TilesetDocument::new("test", "atlas", [16, 16]);
    doc.tiles = vec![
        tile(1, [0, 0, 16, 16], CollisionKind::None),
        tile(2, [16, 0, 16, 16], CollisionKind::Full),
    ];
    for i in 0..16u32 {
        doc.tiles.push(tile(
            3 + i,
            [32 + (i % 4) * 16, (i / 4) * 16, 16, 16],
            CollisionKind::None,
        ));
    }
    doc.rule_tiles = vec![RuleTile::four_neighbour16(100, "path", 3)];
    engine_tilemap::Tileset::from_document(doc)
        .unwrap()
        .to_json()
}
fn level_json(extra: impl FnOnce(&mut Tilemap)) -> String {
    let ts = engine_tilemap::Tileset::from_json(tileset_json().as_bytes()).unwrap();
    let mut map = Tilemap::new(CellRect::new([-64, -64], [64, 64])).unwrap();
    map.layer_mut("ground")
        .unwrap()
        .fill_rect(CellRect::new([-4, -4], [4, 4]), 1, 0, Some(&ts))
        .unwrap();
    let walls = map.add_layer("walls").unwrap();
    walls.opacity = 0.5;
    walls
        .fill_rect(CellRect::new([3, -4], [4, 4]), 2, 0, Some(&ts))
        .unwrap();
    extra(&mut map);
    map.to_json()
}
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(tilemap_data: Value, level: String) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut atlas = image::RgbaImage::new(128, 128);
        for pixel in atlas.pixels_mut() {
            *pixel = image::Rgba([200, 200, 200, 255]);
        }
        atlas.save(dir.path().join("atlas.png")).unwrap();
        fs::write(dir.path().join("tiles.json"), tileset_json()).unwrap();
        fs::write(dir.path().join("level.json"), level).unwrap();
        let scene = json!({"schema":1,"scene_id":"main","entities":[
            {"id":"camera","name":"camera","components":{"Transform":{"position":[0,0,0]},"Camera2D":{}}},
            {"id":"world","name":"world","components":{"Transform":{"position":tilemap_data["at"]},
                "custom":{"engine.Tilemap":{"version":1,"data":tilemap_data["data"]}}}},
            {"id":"player","name":"player","components":{"Transform":{"position":tilemap_data["player"]},
                "Collider2D":{"size":[0.8,0.8]}}}
        ]});
        fs::write(dir.path().join("scene.json"), scene.to_string()).unwrap();
        fs::write(
            dir.path().join("project.json"),
            json!({"schema":1,"name":"tilemap-test","startup_scene":"scene.json","assets":[
                {"id":"atlas","kind":"texture","path":"atlas.png"},
                {"id":"tiles","kind":"tileset","path":"tiles.json"},
                {"id":"level","kind":"tilemap","path":"level.json"}
            ]})
            .to_string(),
        )
        .unwrap();
        Self { dir }
    }
    fn standard() -> Self {
        Self::new(
            json!({"at":[0,0,0],"player":[0.5,0.5,0],"data":{"tileset":"tiles","map":"level"}}),
            level_json(|_| {}),
        )
    }
    fn project(&self) -> Project {
        Project::load(self.dir.path().join("project.json")).unwrap()
    }
    fn runtime(&self) -> Runtime {
        Runtime::new(self.project()).unwrap()
    }
}
fn tick(rt: &mut Runtime) {
    rt.tick(0.1, InputSnapshot::default()).unwrap();
}
fn tile_draws(rt: &Runtime) -> Vec<engine_project::runtime::Draw> {
    rt.extract_frame()
        .unwrap()
        .draws
        .into_iter()
        .filter(|d| d.id.starts_with("mesh:tilemap:world/"))
        .collect()
}

#[test]
fn project_validates_tile_assets_and_component() {
    let fixture = Fixture::standard();
    fixture.project().validate().unwrap();
    // Schema violations are rejected when the project loads.
    let bad = Fixture::new(
        json!({"at":[0,0,0],"player":[0,0,0],"data":{"tileset":"tiles","cell_size":[-1,1]}}),
        level_json(|_| {}),
    );
    assert!(Project::load(bad.dir.path().join("project.json")).is_err());
    let unknown = Fixture::new(
        json!({"at":[0,0,0],"player":[0,0,0],"data":{"tileset":"tiles","unknown":1}}),
        level_json(|_| {}),
    );
    assert!(Project::load(unknown.dir.path().join("project.json")).is_err());
    // Semantic failures (missing tileset asset) surface as a tick diagnostic.
    let missing = Fixture::new(
        json!({"at":[0,0,0],"player":[0,0,0],"data":{"tileset":"nope"}}),
        level_json(|_| {}),
    );
    let mut runtime = missing.runtime();
    let error = runtime.tick(0.1, InputSnapshot::default()).unwrap_err();
    assert!(
        error.to_string().contains("project.asset.unknown"),
        "{error}"
    );
}

#[test]
fn runtime_extracts_tile_geometry_per_chunk() {
    let fixture = Fixture::standard();
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let draws = tile_draws(&rt);
    // Ground covers chunks (-1..0, -1..0) = 4, the wall column covers (0,-1) and (0,0) = 2.
    assert_eq!(
        draws.len(),
        6,
        "{:?}",
        draws.iter().map(|d| &d.id).collect::<Vec<_>>()
    );
    let quads: usize = draws.iter().map(|d| d.mesh.indices.len() / 6).sum();
    assert_eq!(quads, 64 + 8);
    for draw in &draws {
        assert_eq!(draw.space, DrawSpace::World);
        assert_eq!(draw.state.blend, BlendMode::StraightAlpha);
        assert!(!draw.state.depth_test);
        assert!(draw.texture.is_some());
    }
    let walls: Vec<_> = draws.iter().filter(|d| d.id.contains("/walls/")).collect();
    assert_eq!(walls.len(), 2);
    assert!(walls.iter().all(|d| d.layer == 1));
    assert!(
        draws
            .iter()
            .filter(|d| d.id.contains("/ground/"))
            .all(|d| d.layer == 0)
    );
    let xs: Vec<f32> = walls
        .iter()
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.position[0]))
        .collect();
    let ys: Vec<f32> = walls
        .iter()
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.position[1]))
        .collect();
    assert_eq!(xs.iter().cloned().fold(f32::MAX, f32::min), 3.0);
    assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 4.0);
    assert_eq!(ys.iter().cloned().fold(f32::MAX, f32::min), -4.0);
    assert_eq!(ys.iter().cloned().fold(f32::MIN, f32::max), 4.0);
    // Layer opacity 0.5 lands in vertex alpha; the atlas rectangle of tile 2 is [16,0,16,16].
    let v = &walls[0].mesh.vertices[0];
    assert_eq!(v.color, [1.0, 1.0, 1.0, 0.5]);
    assert!(
        walls[0]
            .mesh
            .vertices
            .iter()
            .all(|v| (0.125..=0.25).contains(&v.uv[0]) && v.uv[1] <= 0.125)
    );
    // Ordering: ground draws precede wall draws.
    let frame = rt.extract_frame().unwrap();
    let first_wall = frame
        .draws
        .iter()
        .position(|d| d.id.contains("/walls/"))
        .unwrap();
    let last_ground = frame
        .draws
        .iter()
        .rposition(|d| d.id.contains("/ground/"))
        .unwrap();
    assert!(last_ground < first_wall);
    // Unchanged maps reuse their cached meshes.
    let again = tile_draws(&rt);
    assert!(std::sync::Arc::ptr_eq(&draws[0].mesh, &again[0].mesh));
    // Hiding the component removes every chunk; showing it again rebuilds them.
    let owner = rt.entity("world").unwrap();
    rt.app_mut()
        .world_mut()
        .get_mut::<TilemapComponent>(owner)
        .unwrap()
        .visible = false;
    tick(&mut rt);
    assert!(tile_draws(&rt).is_empty());
    assert_eq!(
        tilemap::status(rt.app().world(), owner)
            .unwrap()
            .render_chunks,
        0
    );
    rt.app_mut()
        .world_mut()
        .get_mut::<TilemapComponent>(owner)
        .unwrap()
        .visible = true;
    tick(&mut rt);
    assert_eq!(tile_draws(&rt).len(), 6);
}

#[test]
fn generated_collision_blocks_a_swept_body() {
    let fixture = Fixture::standard();
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    let status = tilemap::status(rt.app().world(), owner).unwrap();
    // The eight stacked wall cells merge into a single 1 x 8 collider.
    assert_eq!(status.colliders, 1);
    // Player half width 0.4 stops against the wall face at x = 3.
    let result = rt.move_entity("player", [5.0, 0.0]).unwrap();
    assert!(
        (result.position[0] - 2.6).abs() < 1e-4,
        "{:?}",
        result.position
    );
    assert_eq!(result.contacts, vec!["tilemap:world/collider/0".to_owned()]);
    // Open space is not blocked.
    let result = rt.move_entity("player", [-2.0, 0.0]).unwrap();
    assert!(
        (result.position[0] - 0.6).abs() < 1e-4,
        "{:?}",
        result.position
    );
    assert!(result.contacts.is_empty());
}

#[test]
fn runtime_painting_regenerates_render_and_collision() {
    let fixture = Fixture::standard();
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    let world = rt.app_mut().world_mut();
    // Extend the wall upward: still one merged collider, now 1 x 9.
    tilemap::paint(world, owner, "walls", [3, 4], 2, 0).unwrap();
    // Open a doorway at y = 0: the wall splits into two colliders.
    let report = tilemap::erase(world, owner, "walls", [3, 0]).unwrap();
    assert_eq!(report.dirty_chunks, vec![[0, 0]]);
    tick(&mut rt);
    let status = tilemap::status(rt.app().world(), owner).unwrap();
    assert_eq!(status.colliders, 2);
    assert_eq!(status.quads, 64 + 8);
    // The doorway lets the player through while the wall still blocks at another height.
    let result = rt.move_entity("player", [5.0, 0.0]).unwrap();
    assert!(
        (result.position[0] - 5.5).abs() < 1e-4,
        "{:?}",
        result.position
    );
    // Rule tiles autotile at runtime: a three cell path picks the cardinal-mask tiles.
    let world = rt.app_mut().world_mut();
    for x in 0..3 {
        tilemap::paint(world, owner, "ground", [x, -3], 100, 0).unwrap();
    }
    let shown: Vec<u32> = tilemap::with_tilemap(world, owner, |map, _| {
        (0..3)
            .map(|x| map.layer("ground").unwrap().get([x, -3]).shown)
            .collect()
    })
    .unwrap();
    assert_eq!(shown, vec![3 + 2, 3 + 10, 3 + 8]);
    tilemap::fill_rect(world, owner, "ground", CellRect::new([0, 2], [3, 4]), 1, 0).unwrap();
    tilemap::flood_fill(
        world,
        owner,
        "walls",
        [0, 0],
        2,
        0,
        Some(CellRect::new([0, 0], [2, 2])),
    )
    .unwrap();
    tilemap::regenerate_collision(world, owner).unwrap();
    assert_eq!(
        tilemap::status(rt.app().world(), owner).unwrap().colliders,
        3
    );
    // Snapshot export carries runtime edits.
    let document = tilemap::export_document(rt.app_mut().world_mut(), owner).unwrap();
    assert!(document.layers.iter().any(|l| l.name == "walls"));
    tilemap::store_inline(rt.app_mut().world_mut(), owner).unwrap();
    tick(&mut rt);
    assert_eq!(
        tilemap::status(rt.app().world(), owner).unwrap().colliders,
        3
    );
    assert!(rt.snapshot().scene.entities.iter().any(|e| e.id == "world"
        && e.components.custom["engine.Tilemap"].data["inline"]["layers"].is_array()));
}

#[test]
fn world_queries_follow_the_tilemap_transform() {
    let fixture = Fixture::new(
        json!({"at":[10,5,0],"player":[0,-50,0],"data":{"tileset":"tiles","map":"level","cell_size":[2,2]}}),
        level_json(|_| {}),
    );
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    let world = rt.app_mut().world_mut();
    // Cell (3, 2) spans local [6,8) x [4,6), i.e. world [16,18) x [9,11).
    let hit = tilemap::tile_at_world(world, owner, "walls", [17.0, 10.0, 0.0])
        .unwrap()
        .unwrap();
    assert_eq!((hit.cell, hit.tile, hit.shown), ([3, 2], 2, 2));
    assert_eq!(
        hit.properties["kind"],
        engine_tilemap::PropertyValue::Text("test".into())
    );
    assert!(
        tilemap::tile_at_world(world, owner, "walls", [11.0, 6.0, 0.0])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tilemap::world_to_cell(world, owner, [9.9, 4.9, 0.0]).unwrap(),
        [-1, -1]
    );
    assert_eq!(
        tilemap::cell_to_world(world, owner, [3, 2]).unwrap(),
        [17.0, 10.0, 0.0]
    );
    // Render vertices and the generated collider are scaled and placed by the same transform.
    let draws = tile_draws(&rt);
    let walls: Vec<_> = draws.iter().filter(|d| d.id.contains("/walls/")).collect();
    let min_x = walls
        .iter()
        .flat_map(|d| d.mesh.vertices.iter().map(|v| v.position[0]))
        .fold(f32::MAX, f32::min);
    assert_eq!(min_x, 16.0);
    let mut collision_probe = fixture.runtime();
    tick(&mut collision_probe);
    let probe = collision_probe
        .app_mut()
        .world_mut()
        .resource::<engine_project::gameplay2d::World>()
        .body("tilemap:world/collider/0")
        .unwrap();
    assert_eq!(probe.size, [2.0, 16.0]);
    assert_eq!(
        [
            probe.position[0] + probe.offset[0],
            probe.position[1] + probe.offset[1]
        ],
        [17.0, 5.0]
    );
}

#[test]
fn removing_the_owner_cleans_up_generated_entities() {
    let fixture = Fixture::standard();
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    let generated = |rt: &Runtime| {
        rt.app()
            .world()
            .iter_entities()
            .filter(|e| e.contains::<tilemap::TilemapGenerated>())
            .count()
    };
    assert_eq!(generated(&rt), 6 + 1);
    rt.app_mut().world_mut().despawn(owner);
    tick(&mut rt);
    assert_eq!(generated(&rt), 0);
    let result = rt.move_entity("player", [5.0, 0.0]).unwrap();
    assert!((result.position[0] - 5.5).abs() < 1e-4);
}

#[test]
fn collision_options_are_applied_and_capacity_is_reported() {
    let fixture = Fixture::new(
        json!({"at":[0,0,0],"player":[0.5,0.5,0],"data":{"tileset":"tiles","map":"level",
            "collision":{"enabled":false}}}),
        level_json(|_| {}),
    );
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    assert_eq!(
        tilemap::status(rt.app().world(), owner).unwrap().colliders,
        0
    );
    let result = rt.move_entity("player", [5.0, 0.0]).unwrap();
    assert!((result.position[0] - 5.5).abs() < 1e-4);
    // Layer filter: only "ground" (no solid tiles) collides.
    let fixture = Fixture::new(
        json!({"at":[0,0,0],"player":[0.5,0.5,0],"data":{"tileset":"tiles","map":"level",
            "collision":{"layers":["ground"],"layer":4,"mask":2}}}),
        level_json(|_| {}),
    );
    let mut rt = fixture.runtime();
    tick(&mut rt);
    let owner = rt.entity("world").unwrap();
    assert_eq!(
        tilemap::status(rt.app().world(), owner).unwrap().colliders,
        0
    );
    // A 50 x 50 checkerboard needs 1250 separate boxes, over the solver's 1024.
    let dense = level_json(|map| {
        let ts = engine_tilemap::Tileset::from_json(tileset_json().as_bytes()).unwrap();
        let layer = map.layer_mut("walls").unwrap();
        for y in 10..60 {
            for x in 10..60 {
                if (x + y) % 2 == 0 {
                    layer.set([x, y], 2, 0, Some(&ts)).unwrap();
                }
            }
        }
    });
    let fixture = Fixture::new(
        json!({"at":[0,0,0],"player":[0.5,0.5,0],"data":{"tileset":"tiles","map":"level"}}),
        dense,
    );
    let mut rt = fixture.runtime();
    let error = rt.tick(0.1, InputSnapshot::default()).unwrap_err();
    assert!(
        error.to_string().contains("tilemap.collision.capacity"),
        "{error}"
    );
}
