//! Real-adapter tests of the desktop host half of the Hi-Z consumer: authored
//! LOD chains selected per view with hysteresis (before any camera prepass),
//! the selected geometry feeding the certified occluder depth, and the whole
//! authored path from project JSON to GPU-compacted indirect draws.
//!
//! Run with `cargo test -p engine-desktop --test gpu_hiz_lod -- --ignored
//! --test-threads=1 --nocapture`.

use engine_desktop::{OffscreenSprites, RenderLimits, WgpuBackends};
use engine_mesh_lod::ScreenSizeLods;
use engine_project::{
    Camera2D, Project, Runtime,
    content::{Mesh, Vertex},
    rendering3d::*,
    runtime::RenderFrame,
};
use glam::{Mat4, Vec3};
use serde_json::json;
use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;
use std::fs;
use std::sync::Arc;

const SIZE: u32 = 128;
const HIZ: &str = "engine.hiz.visibility";

fn camera(hiz: bool) -> CameraInstance3D {
    CameraInstance3D {
        camera: Camera3D {
            projection: Projection3D::Perspective {
                fov_y_radians: FRAC_PI_2,
                near: 0.1,
                far: 100.0,
            },
            render_features: if hiz {
                BTreeMap::from([(HIZ.to_owned(), true)])
            } else {
                BTreeMap::new()
            },
            ..Default::default()
        },
        world: Mat4::IDENTITY.to_cols_array_2d(),
    }
}

fn frame(hiz: bool) -> SceneRenderFrame {
    SceneRenderFrame {
        indirect: None,
        applied_origin: [0.0; 3],
        mesh_options: Default::default(),
        sprites: RenderFrame {
            camera: Camera2D {
                logical_size: [SIZE, SIZE],
                ..Default::default()
            },
            draws: vec![],
        },
        camera3d: Some(camera(hiz)),
        meshes: vec![],
        lights: vec![],
        post: Default::default(),
        ray_tracing: None,
        materials2d: Default::default(),
        lighting2d: None,
        streams: vec![],
    }
}

/// A quad of half extent `half` with a uniform vertex colour.
fn quad(half: f32, color: [f32; 4]) -> Arc<Mesh> {
    Arc::new(Mesh {
        normals: vec![],
        vertices: [[-half, -half], [half, -half], [half, half], [-half, half]]
            .into_iter()
            .map(|[x, y]| Vertex {
                position: [x, y, 0.0],
                uv: [0.0; 2],
                color,
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    })
}

fn draw(id: &str, mesh: &Arc<Mesh>, position: Vec3, tint: [f32; 4]) -> MeshDraw3D {
    MeshDraw3D {
        id: id.into(),
        mesh: mesh.clone(),
        texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        model: Mat4::from_translation(position).to_cols_array_2d(),
        material: MeshMaterial {
            tint,
            unlit: true,
            ..Default::default()
        },
        shader: None,
    }
}

fn policy(thresholds: [f32; 2]) -> ScreenSizeLods {
    ScreenSizeLods {
        thresholds: thresholds.to_vec(),
        cull_below: None,
        hysteresis: 0.1,
    }
}

fn renderer() -> anyhow::Result<OffscreenSprites> {
    let renderer =
        OffscreenSprites::wgpu(WgpuBackends::Primary, SIZE, SIZE, RenderLimits::default())?;
    eprintln!("{}", renderer.backend_description());
    Ok(renderer)
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    bytes[at..at + 4].try_into().unwrap()
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_lod_levels_switch_with_hysteresis_in_the_rendered_image() -> anyhow::Result<()> {
    let mut renderer = renderer()?;
    // Level zero is red, level one green; the sphere radius of the 1 x 1 base
    // quad is 0.707, so the screen fraction is 0.707 / distance at 90 degrees.
    let fine = quad(0.5, [1.0, 0.0, 0.0, 1.0]);
    let coarse = quad(0.25, [0.0, 1.0, 0.0, 1.0]);
    let mut scene = frame(false);
    scene.mesh_options.insert(
        "lod".into(),
        MeshDrawOptions3D {
            levels: vec![coarse],
            policy: policy([0.5, 0.2]),
            occluder: false,
        },
    );
    scene.meshes = vec![draw("lod", &fine, Vec3::ZERO, [1.0; 4])];
    // (camera distance, expected colour at the centre). Thresholds with 10 %
    // hysteresis: level zero is kept down to 0.45, level one returns above 0.55.
    let steps = [
        (2.0, GREEN), // 0.354
        (1.5, GREEN), // 0.471
        (1.3, GREEN), // 0.544 < 0.55: stays coarse
        (1.2, RED),   // 0.589 >= 0.55
        (1.3, RED),   // 0.544 >= 0.45: stays fine
        (1.5, RED),   // 0.471 >= 0.45
        (1.6, GREEN), // 0.442 < 0.45
        (2.0, GREEN),
    ];
    for (distance, expected) in steps {
        scene.camera3d.as_mut().unwrap().world =
            Mat4::from_translation(Vec3::new(0.0, 0.0, distance)).to_cols_array_2d();
        renderer.render_scene(&scene)?;
        let image = renderer.read_rgba()?;
        let centre = pixel(&image, SIZE / 2, SIZE / 2);
        // Selected geometry, not only colour: the level-one quad has a quarter
        // of the level-zero area.
        let covered = image
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 255 && (p[0] != 0 || p[1] != 0))
            .count();
        eprintln!("distance {distance}: centre {centre:?}, covered pixels {covered}");
        assert_eq!(centre, expected, "distance {distance}");
        let projected = |half: f32| {
            let side = half / distance * SIZE as f32 * 0.5;
            (side * 2.0) * (side * 2.0)
        };
        let want = projected(if expected == RED { 0.5 } else { 0.25 });
        assert!(
            (covered as f32 - want).abs() < want * 0.25 + 40.0,
            "distance {distance}: {covered} px covered, expected about {want}"
        );
    }
    // Draws without a chain are unaffected by the selector.
    scene.mesh_options.clear();
    renderer.render_scene(&scene)?;
    assert_eq!(pixel(&renderer.read_rgba()?, SIZE / 2, SIZE / 2), RED);
    Ok(())
}

/// A wall (marked occluder) with a chain, a small object it always hides, and
/// a ring object that only the coarse wall level fails to cover.
fn occlusion_scene(thresholds: [f32; 2]) -> SceneRenderFrame {
    let mut scene = frame(true);
    let fine = quad(0.6, [1.0, 0.0, 0.0, 1.0]);
    let coarse = quad(0.4, [1.0, 0.0, 0.0, 1.0]);
    let white = [1.0; 4];
    scene.mesh_options.insert(
        "wall".into(),
        MeshDrawOptions3D {
            levels: vec![coarse],
            policy: policy(thresholds),
            occluder: true,
        },
    );
    scene.meshes = vec![
        draw("wall", &fine, Vec3::new(0.0, 0.0, -2.0), white),
        draw(
            "hidden",
            &quad(0.1, white),
            Vec3::new(0.0, 0.0, -5.0),
            [0.0, 1.0, 0.0, 1.0],
        ),
        draw(
            "ring",
            &quad(0.1, white),
            Vec3::new(1.15, 0.0, -5.0),
            [0.0, 0.0, 1.0, 1.0],
        ),
    ];
    scene
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn the_selected_lod_geometry_is_what_occludes_on_the_gpu() -> anyhow::Result<()> {
    let mut renderer = renderer()?;
    // Wall sphere radius 0.85 at distance 2: fraction 0.42. Thresholds
    // [0.5, 0.2] select the coarse level (half extent 0.4); [0.1, 0.05] the
    // fine one (0.6). The ring sits at wall-plane x = 0.46 +- 0.04: covered by
    // the fine wall, uncovered by the coarse one.
    let ring_pixel = (SIZE / 2 + (0.23 * SIZE as f32 * 0.5) as u32 + 1, SIZE / 2);
    let mut results = Vec::new();
    for thresholds in [[0.5, 0.2], [0.1, 0.05]] {
        let scene = occlusion_scene(thresholds);
        let stats = renderer.render_scene(&scene)?;
        let image = renderer.read_rgba()?;
        let diagnostic = renderer
            .gpu_renderer()
            .expect("native renderer")
            .visibility_diagnostic()?
            .expect("compacted indirect camera draws");
        eprintln!(
            "thresholds {thresholds:?}: stats occluders {} candidates {} indirect {}, survivors {}, \
             ring pixel {:?}, centre coverage {}",
            stats.hiz_occluders,
            stats.hiz_candidates,
            stats.hiz_indirect,
            diagnostic.survivors(),
            pixel(&image, ring_pixel.0, ring_pixel.1),
            diagnostic.coverage(SIZE / 2, SIZE / 2),
        );
        assert!(stats.hiz_indirect);
        assert_eq!((stats.hiz_occluders, stats.hiz_candidates), (1, 3));
        assert_eq!(
            diagnostic.coverage(SIZE / 2, SIZE / 2),
            1,
            "the hidden object is never replayed behind the wall"
        );
        results.push((
            diagnostic.survivors(),
            pixel(&image, ring_pixel.0, ring_pixel.1),
            diagnostic.coverage(ring_pixel.0, ring_pixel.1),
        ));
    }
    assert_eq!(
        results[0].0, 2,
        "wall + ring: the coarse wall leaves the ring visible"
    );
    assert_eq!(results[0].1, BLUE);
    assert_eq!(results[0].2, 1);
    assert_eq!(
        results[1].0, 1,
        "the wall alone: the fine wall hides the ring"
    );
    assert_eq!(results[1].1, RED, "and the image shows the wall there");
    assert_eq!(results[1].2, 1, "only the wall is replayed at that pixel");
    Ok(())
}

fn mesh_json(half: f32, color: [f32; 4]) -> Vec<u8> {
    let vertex =
        |x: f32, y: f32| json!({"position": [x, y, 0.0], "uv": [0.0, 0.0], "color": color});
    serde_json::to_vec(&json!({
        "vertices": [vertex(-half, -half), vertex(half, -half), vertex(half, half), vertex(-half, half)],
        "indices": [0, 1, 2, 0, 2, 3]
    }))
    .unwrap()
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn authored_components_reach_gpu_compacted_indirect_draws() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    for (name, half) in [("wall0", 0.6), ("wall1", 0.4), ("small", 0.1)] {
        fs::write(
            dir.path().join(format!("{name}.json")),
            mesh_json(half, [1.0; 4]),
        )?;
    }
    let unlit = |tint: [f32; 4]| json!({"unlit": true, "tint": tint});
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema": 1, "scene_id": "scene", "entities": [
            {"id": "camera", "name": "Camera", "components": {
                "Transform": {"position": [0, 0, 0]},
                "Camera3D": {
                    "projection": {"kind": "perspective", "fov_y_radians": FRAC_PI_2, "near": 0.1, "far": 100.0},
                    "render_features": {HIZ: true}}}},
            {"id": "wall", "name": "Wall", "components": {
                "Transform": {"position": [0, 0, -2]},
                "MeshRenderer": {"mesh": "wall0"},
                "MeshMaterial": unlit([1.0, 0.0, 0.0, 1.0]),
                "MeshLods": {"meshes": ["wall1"], "thresholds": [0.5, 0.2], "hysteresis": 0.1},
                "HiZOccluder": {}}},
            {"id": "hidden", "name": "Hidden", "components": {
                "Transform": {"position": [0, 0, -5]},
                "MeshRenderer": {"mesh": "small"},
                "MeshMaterial": unlit([0.0, 1.0, 0.0, 1.0])}},
            {"id": "ring", "name": "Ring", "components": {
                "Transform": {"position": [1.15, 0, -5]},
                "MeshRenderer": {"mesh": "small"},
                "MeshMaterial": unlit([0.0, 0.0, 1.0, 1.0])}}
        ]}))?,
    )?;
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema": 1, "name": "hiz-authored", "startup_scene": "scene.json", "assets": [
                {"id": "wall0", "kind": "mesh", "path": "wall0.json"},
                {"id": "wall1", "kind": "mesh", "path": "wall1.json"},
                {"id": "small", "kind": "mesh", "path": "small.json"}
            ]}),
        )?,
    )?;
    let runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    let mut authored = runtime.extract_scene_frame()?;
    // The 3D viewport follows the 2D camera's logical size.
    authored.sprites.camera.logical_size = [SIZE, SIZE];
    let mut renderer = renderer()?;
    let stats = renderer.render_scene(&authored)?;
    let image = renderer.read_rgba()?;
    let diagnostic = renderer
        .gpu_renderer()
        .expect("native renderer")
        .visibility_diagnostic()?
        .expect("authored scene uses indirect camera draws");
    eprintln!(
        "authored: stats occluders {} candidates {} indirect {}; survivors {}",
        stats.hiz_occluders,
        stats.hiz_candidates,
        stats.hiz_indirect,
        diagnostic.survivors()
    );
    assert!(stats.hiz_indirect);
    assert_eq!((stats.hiz_occluders, stats.hiz_candidates), (1, 3));
    // The coarse level is selected (fraction 0.42), so the ring survives and the
    // hidden object does not.
    assert_eq!(diagnostic.survivors(), 2, "wall + ring survive");
    assert_eq!(diagnostic.coverage(SIZE / 2, SIZE / 2), 1);
    assert_eq!(pixel(&image, SIZE / 2, SIZE / 2), RED);
    assert_eq!(pixel(&image, SIZE / 2 + 15, SIZE / 2), BLUE);

    // With the feature off the same authored scene is the unchanged direct path
    // and renders the identical image.
    let mut off = authored.clone();
    off.camera3d
        .as_mut()
        .unwrap()
        .camera
        .render_features
        .clear();
    let direct = renderer.render_scene(&off)?;
    assert!(!direct.hiz_indirect && direct.hiz_candidates == 0);
    assert!(
        renderer
            .gpu_renderer()
            .unwrap()
            .visibility_diagnostic()?
            .is_none()
    );
    assert!(
        renderer.read_rgba()? == image,
        "culling does not change the image"
    );
    Ok(())
}
