//! End-to-end authored lightmaps: project JSON (an `indirect` asset with
//! supplied RGB32F lightmaps, a mesh asset with a second UV set, `MeshLightmap`
//! components) -> runtime extraction -> desktop GPU HDR readback.
//! Run explicitly on a real adapter with --ignored --test-threads=1.
use anyhow::Result;
use engine_desktop::*;
use engine_project::{Project, Runtime, rendering3d::SceneRenderFrame};
use serde_json::{Value, json};
use std::fs;

/// Quadrant colours of the 4x4 atlas (top-left, top-right, bottom-left, bottom-right).
const QUADRANTS: [[f32; 3]; 4] = [
    [2.0, 0.5, 0.25],
    [0.25, 3.0, 0.5],
    [0.5, 0.25, 1.5],
    [1.25, 1.0, 0.125],
];
const FLAT: [f32; 3] = [1.0, 0.5, 0.25];
const AMBIENT: f32 = 0.25;
/// Dielectric at roughness 1: baked diffuse is weighted by 1 - F0.
const WEIGHT: f32 = 0.96;

fn texels(values: impl IntoIterator<Item = [f32; 3]>) -> Vec<u8> {
    values
        .into_iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("atlas.rgb32f"),
        texels((0..16).map(|i| QUADRANTS[(i / 8) * 2 + (i % 4) / 2])),
    )
    .unwrap();
    fs::write(root.join("flat.rgb32f"), texels([FLAT])).unwrap();
    // Lightmap-only set: no environment, so the probe term is the camera ambient.
    fs::write(
        root.join("baked.json"),
        serde_json::to_vec(&json!({"version":1,"lightmaps":[
            {"size":[4,4],"path":"atlas.rgb32f"},
            {"size":[1,1],"path":"flat.rgb32f"}]}))
        .unwrap(),
    )
    .unwrap();
    // A square with the UV2 of an unwrapped quad (v down); UV0 is unrelated.
    let vertex = |x: f32, y: f32| json!({"position":[x,y,0],"uv":[0.5,0.5],"color":[1,1,1,1]});
    fs::write(
        root.join("panel.json"),
        serde_json::to_vec(&json!({
            "vertices":[vertex(-0.35,-0.35),vertex(0.35,-0.35),vertex(0.35,0.35),vertex(-0.35,0.35)],
            "normals":[[0,0,1],[0,0,1],[0,0,1],[0,0,1]],
            "uv2":[[0,1],[1,1],[1,0],[0,0]],
            "indices":[0,1,2,0,2,3]}))
        .unwrap(),
    )
    .unwrap();
    let material = json!({"metallic":0,"roughness":1});
    let panel = |id: &str, x: f32, y: f32, lightmap: Option<Value>| {
        let mut components = json!({
            "Transform":{"position":[x,y,0]},"MeshRenderer":{"mesh":"panel"},"MeshMaterial":material});
        if let Some(lightmap) = lightmap {
            components["MeshLightmap"] = lightmap;
        }
        json!({"id":id,"name":id,"components":components})
    };
    let quadrant = |scale_offset: [f32; 2]| json!({"lightmap":0,"scale":[0.5,0.5],"offset":scale_offset,"mode":"replace"});
    let mut entities = vec![json!({"id":"camera","name":"Camera","components":{
        "Transform":{"position":[0,0,3]},
        "Camera2D":{"logicalSize":[64,64]},
        "Camera3D":{"projection":{"kind":"orthographic","height":4,"near":0.1,"far":10},
            "lighting":{"ambient":AMBIENT,"exposure":1,"indirect":"baked"}}}})];
    // Four instances of one mesh asset, one atlas quadrant each.
    for (i, (x, offset)) in [
        (-1.5, [0.0, 0.0]),
        (-0.5, [0.5, 0.0]),
        (0.5, [0.0, 0.5]),
        (1.5, [0.5, 0.5]),
    ]
    .into_iter()
    .enumerate()
    {
        entities.push(panel(&format!("q{i}"), x, 0.0, Some(quadrant(offset))));
    }
    entities.push(panel(
        "added",
        -1.5,
        1.2,
        Some(json!({"lightmap":1,"mode":"add"})),
    ));
    // The unbound panel doubles as the Hi-Z occluder that activates the
    // compacted camera path for the lightmapped batch.
    let mut unbound = panel("unbound", 0.0, -1.2, None);
    unbound["components"]["HiZOccluder"] = json!({});
    entities.push(unbound);
    fs::write(
        root.join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"lightmaps","entities":entities}))
            .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("project.json"),
        serde_json::to_vec(&json!({"schema":1,"name":"authored-lightmaps",
            "startup_scene":"scene.json","assets":[
                {"id":"baked","path":"baked.json","kind":"indirect"},
                {"id":"panel","path":"panel.json","kind":"mesh"}]}))
        .unwrap(),
    )
    .unwrap();
    dir
}

fn hdr(
    renderer: &mut GpuRenderer,
    target: &OffscreenTarget,
    frame: &SceneRenderFrame,
) -> Result<render_pipeline::HdrImage> {
    renderer.render_scene(frame, &target.view, None)?;
    renderer
        .pipeline()?
        .read_scene_hdr(render_pipeline::ViewId::MAIN)
}

/// The pixel under world point (x, y) of the 4-unit orthographic camera.
fn at(image: &render_pipeline::HdrImage, x: f32, y: f32) -> [f32; 4] {
    image.pixel(((x + 2.0) * 16.0) as u32, ((2.0 - y) * 16.0) as u32)
}

#[track_caller]
fn close(actual: [f32; 4], expected: [f32; 3], what: &str) {
    for c in 0..3 {
        let tolerance = 0.01 + expected[c] * 0.01;
        assert!(
            (actual[c] - expected[c]).abs() < tolerance,
            "{what}: {actual:?} vs {expected:?}"
        );
    }
}

#[test]
#[ignore = "requires a real GPU adapter and HDR readback"]
fn authored_lightmaps_sample_atlas_regions_of_one_shared_mesh_end_to_end() -> Result<()> {
    let dir = project();
    let mut runtime = Runtime::new(Project::load(dir.path().join("project.json"))?)?;
    runtime.start()?;
    let frame = runtime.extract_scene_frame()?;
    assert_eq!(frame.indirect.as_ref().unwrap().lightmaps.len(), 2);
    assert_eq!(
        frame.meshes.iter().filter(|m| m.lightmap.is_some()).count(),
        5
    );

    let gpu = render_pipeline::GpuContext::headless_with_options(&WgpuOptions::default())?;
    let target = OffscreenTarget::new(&gpu, 64, 64)?;
    let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default())?;
    let image = hdr(&mut renderer, &target, &frame)?;
    for (i, x) in [-1.5, -0.5, 0.5, 1.5].into_iter().enumerate() {
        let baked = QUADRANTS[i].map(|v| v * WEIGHT);
        close(at(&image, x, 0.0), baked, &format!("instance q{i}"));
    }
    let added = [0, 1, 2].map(|c| AMBIENT + FLAT[c] * WEIGHT);
    close(at(&image, -1.5, 1.2), added, "add mode keeps the ambient");
    close(
        at(&image, 0.0, -1.2),
        [AMBIENT; 3],
        "an unbound mesh keeps the legacy ambient",
    );

    // The Hi-Z compacted camera path draws the same lightmapped pixels.
    let mut hiz = frame.clone();
    hiz.camera3d
        .as_mut()
        .unwrap()
        .camera
        .render_features
        .insert("engine.hiz.visibility".into(), true);
    let stats = renderer.render_scene(&hiz, &target.view, None)?;
    assert!(stats.hiz_indirect, "{stats:?}");
    assert_eq!(stats.hiz_candidates, 6);
    let culled = renderer
        .pipeline()?
        .read_scene_hdr(render_pipeline::ViewId::MAIN)?;
    assert_eq!(culled.pixels, image.pixels, "Hi-Z changed no pixel");

    // Removing the camera's indirect asset is an extraction-time error for
    // bindings, and clearing the bindings restores legacy ambient shading.
    let mut without = frame.clone();
    without.indirect = None;
    without.camera3d.as_mut().unwrap().camera.lighting.indirect = None;
    let error = renderer.render_scene(&without, &target.view, None);
    assert!(error.is_err(), "bindings need an indirect scene");
    for draw in &mut without.meshes {
        draw.lightmap = None;
    }
    let plain = hdr(&mut renderer, &target, &without)?;
    close(at(&plain, -1.5, 0.0), [AMBIENT; 3], "ambient only");
    close(at(&plain, 1.5, 0.0), [AMBIENT; 3], "ambient only");
    Ok(())
}
