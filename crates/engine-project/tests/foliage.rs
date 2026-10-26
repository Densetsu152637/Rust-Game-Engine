//! Authored `engine.Foliage` scene -> Runtime -> generated instances, CPU cluster meshes in the
//! RenderFrame, LOD by camera distance, terrain edits, the GPU render set and assets.
use engine_foliage::{Brush, DensityMap, FoliageType, Impostor, PaintOp, TypeExclusion};
use engine_project::world::foliage::{
    self, FoliageComponent, FoliageGpuRenderer, FoliageRenderSet,
};
use engine_project::world::terrain;
use engine_project::{InputSnapshot, Project, Runtime};
use engine_terrain::{Heightfield, TerrainLayer, asset};
use serde_json::{Value, json};
use std::fs;

const MESH: &str = "CGMESH 1\nvertex -0.2 0 0 0 1\nvertex 0.2 0 0 1 1\nvertex 0 1 0 0.5 0\n";
const MESH_LOW: &str = "CGMESH 1\nvertex -0.2 0 0 0 1\nvertex 0.2 0 0 1 1\nvertex 0 0.5 0 0.5 0\n";
const TEXTURE: &str = "P3\n2 2\n255\n0 255 0 0 255 0\n0 255 0 0 255 0\n";

/// 129 x 129 samples, spacing 1. Flat at 2 m, except a steep ramp on columns 60..=70 (2 m per
/// metre, 63 degrees) leading up to a flat plateau at 22 m, and an 8 x 8 cell hole. The "mud"
/// layer is fully weighted on rows below 40.
fn land() -> Heightfield {
    let side = 129u32;
    let heights = (0..side * side)
        .map(|i| {
            let c = (i % side) as f32;
            if c < 60.0 {
                2.0
            } else if c <= 70.0 {
                2.0 + (c - 60.0) * 2.0
            } else {
                22.0
            }
        })
        .collect();
    let mut field = Heightfield::from_heights(side, side, 1.0, heights).unwrap();
    field
        .add_layer(TerrainLayer::new("mud", [0.4, 0.3, 0.2, 1.0]))
        .unwrap();
    for r in 0..side {
        for c in 0..side {
            let mud = r < 40;
            field
                .set_sample_weights(c, r, if mud { &[0, 255] } else { &[255, 0] })
                .unwrap();
        }
    }
    for r in 20..28 {
        for c in 20..28 {
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

fn grass() -> FoliageType {
    let mut t = FoliageType::new("grass", "grass");
    t.density = 0.5;
    t.slope_degrees = [0.0, 30.0];
    t.lod_meshes = vec!["grass_low".into()];
    t.lod_distances = vec![40.0];
    t.impostor = Some(Impostor {
        texture: "card".into(),
        size: [1.0, 1.0],
    });
    t.impostor_distance = Some(90.0);
    t.cull_distance = Some(160.0);
    t
}

fn foliage_data(types: Vec<FoliageType>) -> Value {
    json!({
        "types": types,
        "terrain": "land",
        "seed": 5,
        "center": [64.0, 128.0, 64.0],
        "size": [128.0, 256.0, 128.0],
        "render_mode": "cpu",
        "culling": {"max_distance": 200.0, "hysteresis": 0.1}
    })
}

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(foliage: Value, extra: Vec<Value>, extra_assets: Vec<Value>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("land.terrain"), asset::encode(&land())).unwrap();
        fs::write(dir.path().join("grass.cgmesh"), MESH).unwrap();
        fs::write(dir.path().join("grass_low.cgmesh"), MESH_LOW).unwrap();
        fs::write(dir.path().join("card.ppm"), TEXTURE).unwrap();
        let mut entities = vec![
            entity("camera", [30.0, 8.0, 30.0], json!({"Camera3D": {}})),
            entity(
                "land",
                [0.0, 0.0, 0.0],
                custom(
                    "engine.Terrain",
                    json!({"asset":"land","lod":{"node_resolution":16},"render_mode":"cpu"}),
                ),
            ),
            entity("plants", [0.0, 0.0, 0.0], custom("engine.Foliage", foliage)),
        ];
        entities.extend(extra);
        fs::write(
            dir.path().join("scene.json"),
            json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
        )
        .unwrap();
        let mut assets = vec![
            json!({"id":"land","kind":"terrain","path":"land.terrain"}),
            json!({"id":"grass","kind":"mesh","path":"grass.cgmesh"}),
            json!({"id":"grass_low","kind":"mesh","path":"grass_low.cgmesh"}),
            json!({"id":"card","kind":"texture","path":"card.ppm"}),
        ];
        assets.extend(extra_assets);
        fs::write(
            dir.path().join("project.json"),
            json!({"schema":1,"name":"foliage-test","startup_scene":"scene.json","assets":assets})
                .to_string(),
        )
        .unwrap();
        Self { dir }
    }
    fn standard() -> Self {
        Self::new(foliage_data(vec![grass()]), vec![], vec![])
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
fn move_camera(runtime: &mut Runtime, position: [f32; 3]) {
    let camera = runtime.entity("camera").unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<engine_project::Transform>(camera)
        .unwrap()
        .position = position;
}
fn plants(runtime: &Runtime) -> Vec<engine_project::rendering3d::MeshDraw3D> {
    runtime
        .extract_scene_frame()
        .unwrap()
        .meshes
        .into_iter()
        .filter(|m| m.id.starts_with("foliage:plants/"))
        .collect()
}
fn edit(runtime: &mut Runtime, change: impl FnOnce(&mut FoliageComponent)) {
    let entity = runtime.entity("plants").unwrap();
    change(
        &mut runtime
            .app_mut()
            .world_mut()
            .get_mut::<FoliageComponent>(entity)
            .unwrap(),
    );
}

#[test]
fn project_validates_the_asset_kind_the_component_and_its_references() {
    let fixture = Fixture::standard();
    fixture.project().validate().unwrap();
    let mut bad = grass();
    bad.density = 100.0;
    let bad = Fixture::new(foliage_data(vec![bad]), vec![], vec![]);
    assert!(Project::load(bad.dir.path().join("project.json")).is_err());
    let mut missing = grass();
    missing.mesh = "nope".into();
    let missing = Fixture::new(foliage_data(vec![missing]), vec![], vec![]);
    let error = Project::load(missing.dir.path().join("project.json"))
        .and_then(Runtime::new)
        .err()
        .expect("unknown mesh asset");
    assert!(error.to_string().contains("nope"), "{error}");
    // Asset mode needs an existing foliage asset.
    let asset_mode = Fixture::new(
        json!({"mode":"asset","instances":"missing","terrain":"land"}),
        vec![],
        vec![],
    );
    assert!(Project::load(asset_mode.dir.path().join("project.json")).is_err());
}

#[test]
fn generated_instances_respect_slope_holes_and_the_ground() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(status.diagnostics.is_empty(), "{status:?}");
    assert_eq!(status.ground, "terrain:land");
    // Density 0.5 / m2 over the 128 x 128 box, minus the ramp (11 m), the hole (64 m2) and
    // the guard of the steep band: roughly 0.5 * (118 * 128 - 64) = 7500.
    assert!(
        (6900..8100).contains(&status.instances),
        "instances {}",
        status.instances
    );
    let field = foliage::field(runtime.app_mut().world_mut(), entity).unwrap();
    let terrain_entity = runtime.entity("land").unwrap();
    let heightfield = terrain::heightfield(runtime.app_mut().world_mut(), terrain_entity).unwrap();
    for i in field.iter() {
        let (x, z) = (i.position[0], i.position[2]);
        assert!(
            !heightfield.is_hole_at(x, z),
            "instance in the hole at ({x}, {z})"
        );
        assert!(
            heightfield.slope_degrees_at(x, z) <= 30.0 + 1e-3,
            "slope {} at ({x}, {z})",
            heightfield.slope_degrees_at(x, z)
        );
        let ground = heightfield.height_at(x, z);
        assert!(
            (i.position[1] - ground).abs() < 1e-3,
            "{} vs {ground}",
            i.position[1]
        );
    }
    // The steep band (columns 60..70) and the hole stay empty.
    assert!(field.iter().all(|i| !(60.5..69.5).contains(&i.position[0])));
}

#[test]
fn the_render_frame_has_foliage_geometry_that_changes_with_camera_distance() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let near = foliage::status(runtime.app().world(), entity).unwrap();
    assert_eq!(near.render_path, "cpu-clusters");
    let near_meshes = plants(&runtime);
    assert_eq!(near_meshes.len(), near.cluster_entities);
    assert!(
        near.cluster_levels[0] > 0,
        "full detail near the camera: {near:?}"
    );
    assert!(
        near.cluster_levels[1] > 0,
        "mesh LOD 1 further out: {near:?}"
    );
    let near_vertices: usize = near_meshes.iter().map(|m| m.mesh.vertices.len()).sum();
    assert_eq!(near_vertices, near.cpu_vertices);
    assert!(near_vertices > 1000);
    // Move far away: only impostors (two crossed quads = 8 vertices per instance), then nothing.
    move_camera(&mut runtime, [64.0, 8.0, 64.0 + 120.0]);
    tick(&mut runtime, 1);
    let mid = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(mid.cluster_levels[4] > 0, "impostors at 120 m: {mid:?}");
    assert_ne!(mid.cpu_vertices, near.cpu_vertices);
    assert!(plants(&runtime).iter().any(|m| m.material.transparent));
    move_camera(&mut runtime, [64.0, 8.0, 64.0 + 600.0]);
    tick(&mut runtime, 1);
    let far = foliage::status(runtime.app().world(), entity).unwrap();
    assert_eq!(far.cluster_entities, 0, "{far:?}");
    assert!(plants(&runtime).is_empty());
    // Coming back restores the geometry.
    move_camera(&mut runtime, [30.0, 8.0, 30.0]);
    tick(&mut runtime, 1);
    let back = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(
        back.cpu_vertices > 0 && back.cluster_levels[0] > 0,
        "{back:?}"
    );
}

#[test]
fn render_geometry_never_appears_on_steep_slopes_or_holes() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    // The hole shrunk by the blade half width: a blade rooted outside may overhang the edge.
    let hole = |x: f32, z: f32| (20.5..27.5).contains(&x) && (20.5..27.5).contains(&z);
    let mut checked = 0;
    for mesh in plants(&runtime) {
        for v in &mesh.mesh.vertices {
            // Blade bases sit on the ground (y = height); only look at base vertices (the
            // clusters are in world space, the draw model is identity).
            let [x, y, z] = v.position;
            if (y - 2.0).abs() < 1e-3 || y > 2.0 {
                checked += 1;
                assert!(
                    !hole(x, z) || y > 2.5,
                    "foliage inside the hole at ({x}, {z})"
                );
            }
            assert!(
                !(61.0..69.0).contains(&x),
                "foliage on the steep band at x = {x}"
            );
        }
    }
    assert!(checked > 1000);
}

#[test]
fn generation_is_deterministic_and_follows_input_changes() {
    let fixture = Fixture::standard();
    let mut a = fixture.runtime();
    let mut b = fixture.runtime();
    tick(&mut a, 1);
    tick(&mut b, 1);
    let (ea, eb) = (a.entity("plants").unwrap(), b.entity("plants").unwrap());
    let fa = foliage::field(a.app_mut().world_mut(), ea).unwrap();
    let fb = foliage::field(b.app_mut().world_mut(), eb).unwrap();
    assert_eq!(
        fa.iter().copied().collect::<Vec<_>>(),
        fb.iter().copied().collect::<Vec<_>>()
    );
    // Density change regenerates; the count scales with it.
    let before = fa.len();
    edit(&mut a, |c| c.types[0].density = 1.0);
    tick(&mut a, 1);
    let doubled = foliage::status(a.app().world(), ea).unwrap();
    assert_eq!(doubled.attempts, 2);
    assert!(
        (doubled.instances as f32 / before as f32 - 2.0).abs() < 0.08,
        "{} vs {before}",
        doubled.instances
    );
    // A new seed gives a different arrangement with a similar count.
    edit(&mut a, |c| c.seed = 99);
    tick(&mut a, 1);
    let other = foliage::field(a.app_mut().world_mut(), ea).unwrap();
    assert_ne!(
        other.iter().next().map(|i| i.position),
        fa.iter().next().map(|i| i.position)
    );
    // Without a change nothing is regenerated.
    let attempts = foliage::status(a.app().world(), ea).unwrap().attempts;
    tick(&mut a, 3);
    assert_eq!(
        foliage::status(a.app().world(), ea).unwrap().attempts,
        attempts
    );
    // Render-only edits do not regenerate either.
    edit(&mut a, |c| c.culling.max_distance = 300.0);
    tick(&mut a, 1);
    assert_eq!(
        foliage::status(a.app().world(), ea).unwrap().attempts,
        attempts
    );
}

#[test]
fn density_follows_a_terrain_paint_layer_and_exclusions_apply() {
    let mut mud_grass = grass();
    mud_grass.name = "reeds".into();
    mud_grass.density = 1.0;
    mud_grass.slope_degrees = [0.0, 90.0];
    mud_grass.density_map = DensityMap::Layer {
        layer: "mud".into(),
        invert: false,
        threshold: 0.0,
    };
    let mut bush = grass();
    bush.name = "bush".into();
    bush.density = 0.2;
    bush.excludes = vec![TypeExclusion {
        name: "reeds".into(),
        radius: 1.5,
    }];
    let fixture = Fixture::new(foliage_data(vec![mud_grass, bush]), vec![], vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let field = foliage::field(runtime.app_mut().world_mut(), entity).unwrap();
    let reeds: Vec<_> = field.iter().filter(|i| i.type_index == 0).collect();
    let bushes: Vec<_> = field.iter().filter(|i| i.type_index == 1).collect();
    // Mud covers rows 0..40 (weight interpolates between rows 39 and 40): reeds only there.
    assert!(reeds.iter().all(|i| i.position[2] <= 40.0 + 1e-3));
    assert!(reeds.len() > 3000, "{}", reeds.len());
    assert!(bushes.iter().any(|i| i.position[2] > 41.0));
    for b in &bushes {
        assert!(reeds.iter().all(|r| {
            (r.position[0] - b.position[0]).hypot(r.position[2] - b.position[2]) >= 1.5
        }));
    }
}

#[test]
fn instances_follow_terrain_edits() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let terrain_entity = runtime.entity("land").unwrap();
    let before = foliage::status(runtime.app().world(), entity).unwrap();
    // Raise a hill under the camera area (terrain-local == world here).
    terrain::sculpt(
        runtime.app_mut().world_mut(),
        terrain_entity,
        &engine_terrain::Brush {
            center: [30.0, 30.0],
            radius: 12.0,
            strength: 6.0,
            falloff: 0.0,
        },
        engine_terrain::SculptOp::Raise,
    )
    .unwrap();
    tick(&mut runtime, 2);
    let after = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(after.attempts > before.attempts);
    assert_ne!(after.revision, before.revision);
    let field = foliage::field(runtime.app_mut().world_mut(), entity).unwrap();
    let heightfield = terrain::heightfield(runtime.app_mut().world_mut(), terrain_entity).unwrap();
    let mut lifted = 0;
    for i in field.iter() {
        let ground = heightfield.height_at(i.position[0], i.position[2]);
        assert!((i.position[1] - ground).abs() < 1e-3);
        if (i.position[0] - 30.0).hypot(i.position[2] - 30.0) < 6.0 {
            assert!(i.position[1] > 7.9, "inside the hill: {}", i.position[1]);
            lifted += 1;
        }
    }
    assert!(lifted > 20, "{lifted}");
}

#[test]
fn painted_instances_export_to_an_asset_that_reloads_and_reprojects() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 1);
    let entity = runtime.entity("plants").unwrap();
    // Erase a disc, then paint a denser patch of a new seed.
    let erased = foliage::paint(
        runtime.app_mut().world_mut(),
        entity,
        &Brush::new([30.0, 100.0], 10.0),
        PaintOp::Erase { type_index: None },
        1,
    )
    .unwrap();
    assert!(erased.removed > 40, "{erased:?}");
    let field = foliage::field(runtime.app_mut().world_mut(), entity).unwrap();
    assert!(
        field
            .iter()
            .all(|i| (i.position[0] - 30.0).hypot(i.position[2] - 100.0) > 5.0 - 1e-3)
    );
    let painted = foliage::paint(
        runtime.app_mut().world_mut(),
        entity,
        &Brush {
            center: [100.0, 100.0],
            radius: 6.0,
            strength: 1.0,
            falloff: 0.0,
        },
        PaintOp::Add { type_index: 0 },
        77,
    )
    .unwrap();
    assert!(
        painted.added > 50,
        "a new seed adds a fresh arrangement: {painted:?}"
    );
    // Painting with the generation seed re-draws the generated candidates: nothing new.
    let again = foliage::paint(
        runtime.app_mut().world_mut(),
        entity,
        &Brush::new([60.0, 100.0], 6.0),
        PaintOp::Add { type_index: 0 },
        5,
    )
    .unwrap();
    assert_eq!(again.added, 0, "{again:?}");
    let count = foliage::field(runtime.app_mut().world_mut(), entity)
        .unwrap()
        .len();
    let bytes = foliage::export_asset(runtime.app_mut().world_mut(), entity).unwrap();
    // A second project that loads the exported asset.
    let asset_fixture = Fixture::new(
        json!({"mode":"asset","instances":"baked","terrain":"land","render_mode":"cpu"}),
        vec![],
        vec![json!({"id":"baked","kind":"foliage","path":"baked.foliage"})],
    );
    fs::write(asset_fixture.dir.path().join("baked.foliage"), &bytes).unwrap();
    asset_fixture.project().validate().unwrap();
    let mut loaded = asset_fixture.runtime();
    tick(&mut loaded, 2);
    let e = loaded.entity("plants").unwrap();
    let status = foliage::status(loaded.app().world(), e).unwrap();
    assert!(status.diagnostics.is_empty(), "{status:?}");
    assert_eq!(status.instances, count);
    assert!(status.cluster_entities > 0);
    // Raising the terrain re-projects the loaded instances onto the new ground.
    let terrain_entity = loaded.entity("land").unwrap();
    terrain::sculpt(
        loaded.app_mut().world_mut(),
        terrain_entity,
        &engine_terrain::Brush {
            center: [30.0, 30.0],
            radius: 12.0,
            strength: 5.0,
            falloff: 0.0,
        },
        engine_terrain::SculptOp::Raise,
    )
    .unwrap();
    tick(&mut loaded, 2);
    let field = foliage::field(loaded.app_mut().world_mut(), e).unwrap();
    let heightfield = terrain::heightfield(loaded.app_mut().world_mut(), terrain_entity).unwrap();
    assert!(field.len() <= count);
    assert!(field.iter().all(|i| {
        (i.position[1] - heightfield.height_at(i.position[0], i.position[2])).abs() < 1e-3
    }));
    assert!(field.iter().any(|i| i.position[1] > 6.0));
}

#[test]
fn the_gpu_marker_replaces_cpu_clusters_with_a_render_set() {
    let fixture = Fixture::new(
        {
            let mut data = foliage_data(vec![grass()]);
            data["render_mode"] = json!("auto");
            data
        },
        vec![],
        vec![],
    );
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    // Without a GPU renderer `auto` is the CPU fallback.
    assert_eq!(
        foliage::status(runtime.app().world(), entity)
            .unwrap()
            .render_path,
        "cpu-clusters"
    );
    assert!(!plants(&runtime).is_empty());
    assert!(
        runtime
            .app()
            .world()
            .resource::<FoliageRenderSet>()
            .items
            .is_empty()
    );
    // With the marker nothing is drawn twice: no clusters, one render item.
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(FoliageGpuRenderer);
    tick(&mut runtime, 1);
    assert!(plants(&runtime).is_empty());
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert_eq!(status.render_path, "gpu-feature");
    assert_eq!(status.cluster_entities, 0);
    let set = runtime.app().world().resource::<FoliageRenderSet>();
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "plants");
    assert_eq!(set.items[0].field.len(), status.instances);
    assert_eq!(set.items[0].types[0].meshes.len(), 2);
    assert!(set.items[0].types[0].impostor.is_some());
    // Forcing cpu draws clusters again and leaves the render set.
    edit(&mut runtime, |c| {
        c.render_mode = foliage::FoliageRenderMode::Cpu
    });
    tick(&mut runtime, 1);
    assert!(!plants(&runtime).is_empty());
    assert!(
        runtime
            .app()
            .world()
            .resource::<FoliageRenderSet>()
            .items
            .is_empty()
    );
    // `gpu` without a renderer draws nothing and says why.
    runtime
        .app_mut()
        .world_mut()
        .remove_resource::<FoliageGpuRenderer>();
    edit(&mut runtime, |c| {
        c.render_mode = foliage::FoliageRenderMode::Gpu
    });
    tick(&mut runtime, 1);
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(
        status
            .notes
            .iter()
            .any(|n| n.contains("no foliage GPU renderer")),
        "{status:?}"
    );
}

#[test]
fn the_cpu_vertex_budget_bounds_the_merged_meshes_nearest_first() {
    let fixture = Fixture::standard();
    let mut runtime = fixture.runtime();
    edit(&mut runtime, |c| c.cpu_vertex_budget = 3000);
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(status.budget_limited, "{status:?}");
    assert!(status.cpu_vertices <= 3000, "{}", status.cpu_vertices);
    assert!(status.cpu_vertices > 2500, "{}", status.cpu_vertices);
    let total: usize = plants(&runtime).iter().map(|m| m.mesh.vertices.len()).sum();
    assert_eq!(total, status.cpu_vertices);
    // The nearest cell (the camera stands over it) is kept.
    assert!(plants(&runtime).iter().any(|m| m.id.contains("/cell/0,0/")));
}

#[test]
fn a_missing_ground_reports_a_diagnostic_and_recovers_when_it_appears() {
    let mut data = foliage_data(vec![grass()]);
    // `later` exists (references must resolve) but is not a terrain yet.
    data["terrain"] = json!("later");
    let fixture = Fixture::new(data, vec![entity("later", [0.0; 3], json!({}))], vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 2);
    let entity = runtime.entity("plants").unwrap();
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert_eq!(status.instances, 0);
    assert!(
        status
            .diagnostics
            .iter()
            .any(|d| d.contains("foliage.ground.terrain")),
        "{status:?}"
    );
    // Pointing it at the real terrain generates.
    edit(&mut runtime, |c| c.terrain = "land".into());
    tick(&mut runtime, 2);
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert!(status.diagnostics.is_empty(), "{status:?}");
    assert!(status.instances > 1000);
}

#[test]
fn foliage_on_a_pcg_surface_name_uses_the_registry() {
    let mut data = foliage_data(vec![grass()]);
    data["terrain"] = json!("");
    data["surface"] = json!("terrain");
    let fixture = Fixture::new(data, vec![], vec![]);
    let mut runtime = fixture.runtime();
    tick(&mut runtime, 3);
    let entity = runtime.entity("plants").unwrap();
    let status = foliage::status(runtime.app().world(), entity).unwrap();
    assert_eq!(status.ground, "surface:terrain");
    // The registry surface has no hole information, so the hole is planted over, but the
    // slope limit still holds.
    assert!(status.instances > 1000, "{status:?}");
    let field = foliage::field(runtime.app_mut().world_mut(), entity).unwrap();
    assert!(field.iter().all(|i| !(60.5..69.5).contains(&i.position[0])));
}
