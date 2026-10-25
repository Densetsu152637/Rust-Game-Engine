//! GPU tests of the UI render features on a real adapter (Vulkan on the
//! reference machine). Ignored by default; run with
//! `cargo test -p engine-ui-render --test gpu_ui -- --ignored --test-threads=1`.
//!
//! Set `UI_RENDER_PNG_DIR` to also write the rendered images as PNGs for
//! inspection (they are not committed).
use engine_desktop::*;
use engine_project::{
    Project, Runtime,
    accessibility::{ColorFilterKind, ColorFilterMode, ColorFilterSettings},
    ui_tree::{UiFeatureState, UiGpuFeature, UiGpuQueue, UiRenderStatus},
};
use engine_render_pipeline::PipelineFeatures;
use engine_ui_render::{
    UiRenderOptions,
    color::{apply_linear, linear_to_srgb, srgb_to_linear},
    register_with,
};
use serde_json::{Value, json};
use std::fs;

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
/// A unit square in the XY plane, as a CGMESH (two triangles).
const QUAD: &str = "CGMESH 1\nvertex -1 -1 0 0 1\nvertex 1 -1 0 1 1\nvertex 1 1 0 1 0\n\
                    vertex -1 -1 0 0 1\nvertex 1 1 0 1 0\nvertex -1 1 0 0 0\n";

struct Scene {
    dir: tempfile::TempDir,
}

impl Scene {
    fn new(entities: Vec<Value>, font_atlas: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ui.ttf"), UBUNTU).unwrap();
        fs::write(dir.path().join("quad.cgmesh"), QUAD).unwrap();
        fs::write(
            dir.path().join("fonts.json"),
            json!({"schema":1,"defaultFamily":"ui","families":{"ui":{"size":16,"atlas":font_atlas}}})
                .to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("project.json"),
            json!({"schema":1,"name":"UI GPU","startup_scene":"scene.json","assets":[
                {"id":"ui","kind":"font_face","path":"ui.ttf"},
                {"id":"fonts","kind":"font_config","path":"fonts.json"},
                {"id":"quad","kind":"mesh","path":"quad.cgmesh"}]})
            .to_string(),
        )
        .unwrap();
        fs::write(
            dir.path().join("scene.json"),
            json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
        )
        .unwrap();
        Self { dir }
    }

    fn runtime(&self) -> Runtime {
        let project = Project::load(self.dir.path().join("project.json")).unwrap();
        let mut runtime = Runtime::new(project).unwrap();
        runtime.start().unwrap();
        runtime
    }
}

/// A headless renderer with the chosen UI features prepared on its device.
struct Gpu {
    renderer: GpuRenderer,
    target: OffscreenTarget,
    size: [u32; 2],
}

impl Gpu {
    fn new(size: [u32; 2], runtime: &Runtime, options: Option<UiRenderOptions>) -> Self {
        let gpu = GpuContext::headless_with_options(&WgpuOptions {
            backends: WgpuBackends::Primary,
            ..Default::default()
        })
        .expect("a native GPU adapter");
        let target = OffscreenTarget::new(&gpu, size[0], size[1]).unwrap();
        let mut renderer = GpuRenderer::new(gpu, target.info, RenderLimits::default()).unwrap();
        if let Some(options) = options {
            let queue = runtime.app().world().resource::<UiGpuQueue>().clone();
            let mut features = PipelineFeatures::default();
            register_with(&mut features, &queue, options).unwrap();
            renderer.set_features(features).unwrap();
        }
        Self {
            renderer,
            target,
            size,
        }
    }

    /// Extract (after the features are prepared, so routing is active) and render.
    fn render(&mut self, runtime: &Runtime) -> Image {
        let frame = runtime.extract_scene_frame().unwrap();
        self.renderer
            .render_scene(
                &frame,
                &self.target.view,
                Some(runtime.app().render_world()),
            )
            .unwrap();
        let rgba = self.target.read_rgba(&self.renderer.gpu).unwrap();
        Image {
            size: self.size,
            rgba,
        }
    }
}

struct Image {
    size: [u32; 2],
    rgba: Vec<u8>,
}

impl Image {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.size[0] + x) * 4) as usize;
        self.rgba[i..i + 4].try_into().unwrap()
    }

    /// Linear luminance (0..1) of a pixel from the sRGB-encoded target.
    fn linear_luma(&self, x: u32, y: u32) -> f32 {
        let p = self.at(x, y);
        srgb_to_linear(f32::from(p[0]) / 255.0) * 0.2126
            + srgb_to_linear(f32::from(p[1]) / 255.0) * 0.7152
            + srgb_to_linear(f32::from(p[2]) / 255.0) * 0.0722
    }

    fn save(&self, name: &str) {
        let Ok(dir) = std::env::var("UI_RENDER_PNG_DIR") else {
            return;
        };
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        image::save_buffer(
            path,
            &self.rgba,
            self.size[0],
            self.size[1],
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    /// Count of pixels whose colour is within `tolerance` of `rgb` per channel.
    fn count_near(&self, rgb: [u8; 3], tolerance: u8) -> usize {
        self.rgba
            .chunks(4)
            .filter(|p| (0..3).all(|c| p[c].abs_diff(rgb[c]) <= tolerance))
            .count()
    }

    /// Width in pixels of the first rising edge of a row, between 10% and 90% of
    /// the row's peak linear luminance, with linear interpolation.
    fn edge_width(&self, y: u32) -> Option<f32> {
        let row: Vec<f32> = (0..self.size[0]).map(|x| self.linear_luma(x, y)).collect();
        let peak = row.iter().copied().fold(0.0, f32::max);
        if peak < 0.5 {
            return None;
        }
        // First pixel above `level`, interpolated against its left neighbour.
        let crossing = |level: f32| -> Option<f32> {
            let i = row.iter().position(|v| *v > level)?;
            let (a, b) = (row[i.checked_sub(1)?], row[i]);
            Some(i as f32 - 1.0 + (level - a) / (b - a))
        };
        Some(crossing(0.9 * peak)? - crossing(0.1 * peak)?)
    }

    /// Rows containing a pixel brighter than half the peak, as (top, bottom).
    fn lit_rows(&self) -> Option<(u32, u32)> {
        let lit: Vec<u32> = (0..self.size[1])
            .filter(|&y| (0..self.size[0]).any(|x| self.linear_luma(x, y) > 0.5))
            .collect();
        Some((*lit.first()?, *lit.last()?))
    }
}

fn camera(logical: [u32; 2], extra: Value) -> Value {
    json!({"id":"camera","name":"Camera","components":{
        "Transform":{"position":[0,0,4]},
        "Camera3D": extra,
        "Camera2D":{"logicalSize":logical}}})
}

// ---- (a) SDF text -------------------------------------------------------------------

fn hud(font_atlas: &str) -> Scene {
    // One large "I": a straight stem whose side edges show the transition width.
    Scene::new(
        vec![
            json!({"id":"cam2d","name":"Camera","components":{"Camera2D":{"logicalSize":[320,180]}}}),
            json!({"id":"hud","name":"HUD","components":{"custom":{"engine.UiRoot":{"version":1,"data":{
            "version":1,"tree":{"nodes":[
                {"id":"stem","widget":{"kind":"label","text":"I"},"style":{
                    "font":"ui","fontScale":4,"padding":0,
                    "background":[0,0,0,1],"foreground":[1,1,1,1],
                    "width":{"unit":"pixels","value":60},"height":{"unit":"pixels","value":100}}}
            ]}}}}}}),
        ],
        font_atlas,
    )
}

/// Edge widths (10% to 90%, pixels) of the stem at one fit scale.
fn stem_edge(font_atlas: &str, scale: u32, name: &str) -> (f32, UiRenderStatus) {
    let scene = hud(font_atlas);
    let runtime = scene.runtime();
    let size = [320 * scale, 180 * scale];
    let mut gpu = Gpu::new(size, &runtime, Some(UiRenderOptions::default()));
    let image = gpu.render(&runtime);
    image.save(name);
    let (top, bottom) = image.lit_rows().expect("the stem is drawn");
    let width = image
        .edge_width((top + bottom) / 2)
        .expect("a rising edge on the middle row");
    // Read the status while the renderer (and so the features) is alive.
    let status = runtime.app().world().resource::<UiGpuQueue>().status();
    (width, status)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn sdf_text_edges_stay_about_one_pixel_wide_at_two_scales() {
    let mut report = String::new();
    for scale in [2u32, 4] {
        let (sdf, status) = stem_edge("sdf", scale, &format!("sdf_text_x{scale}"));
        let (coverage, _) = stem_edge("coverage", scale, &format!("coverage_text_x{scale}"));
        report +=
            &format!("scale {scale}x: sdf edge {sdf:.2} px, coverage edge {coverage:.2} px\n");
        assert!(sdf <= 2.0, "SDF edge is {sdf} px wide at {scale}x");
        assert!(
            sdf < coverage,
            "coverage text magnified {scale}x is blurrier ({coverage} px) than SDF ({sdf} px)"
        );
        let sdf_feature = status.feature(UiGpuFeature::SdfText).unwrap();
        assert_eq!(sdf_feature.state, UiFeatureState::Gpu, "{sdf_feature:?}");
        assert!(sdf_feature.frames >= 1 && sdf_feature.last_draws >= 1);
    }
    println!("{report}");
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn sdf_text_reports_the_gpu_path_and_the_coverage_fallback_when_unregistered() {
    let scene = hud("sdf");
    // Without the features: fallback with the registration reason, text still draws.
    let runtime = scene.runtime();
    let mut gpu = Gpu::new([640, 360], &runtime, None);
    let image = gpu.render(&runtime);
    assert!(
        image.count_near([255, 255, 255], 8) > 100,
        "coverage text draws"
    );
    let status = runtime.text_engine().unwrap().render_status();
    assert_eq!(
        status.state,
        engine_project::ui_tree::TextRenderState::SdfFallback
    );
    assert!(status.fallback_reason.unwrap().contains("not registered"));
    // With the features prepared on the device and one frame drawn: Sdf.
    let runtime = scene.runtime();
    let mut gpu = Gpu::new([640, 360], &runtime, Some(UiRenderOptions::default()));
    let image = gpu.render(&runtime);
    assert!(image.count_near([255, 255, 255], 8) > 100, "SDF text draws");
    let status = runtime.text_engine().unwrap().render_status();
    // No 3D camera in this scene: the world path reports its own fallback.
    assert_eq!(status.screen.state, UiFeatureState::Gpu, "{status:?}");
    assert!(status.sdf_gpu_layouts >= 1);
}

// ---- (b) world panels in 3D ---------------------------------------------------------

fn panel(id: &str, position: [f32; 3], rotation: [f32; 4], color: [f32; 3], extra: Value) -> Value {
    let mut data = json!({"size":[4,2],"pixelsPerUnit":50});
    data.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"id":id,"name":id,"components":{
        "Transform":{"position":position,"rotation":rotation},
        "custom":{
            "engine.UiRoot":{"version":1,"data":{"version":1,"tree":{"nodes":[
                {"id":"fill","style":{"padding":0,
                    "background":[color[0],color[1],color[2],1],
                    "width":{"unit":"fill"},"height":{"unit":"fill"}}}]}}},
            "engine.UiWorldPanel":{"version":1,"data":data}}}})
}

fn occluder(id: &str, position: [f32; 3], scale: [f32; 3]) -> Value {
    json!({"id":id,"name":id,"components":{
        "Transform":{"position":position,"scale":scale},
        "MeshRenderer":{"mesh":"quad"},
        "MeshMaterial":{"tint":[1,0,0,1],"unlit":true}}})
}

const GREEN: [u8; 4] = [0, 255, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

fn occlusion_image(occluder_z: f32) -> Image {
    // A green 4 x 2 panel at the origin; a red opaque quad covering the left
    // half of the view (x in [-3, 0]) either in front of it or behind it.
    let scene = Scene::new(
        vec![
            camera([128, 128], json!({})),
            panel(
                "panel",
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0],
                json!({}),
            ),
            occluder("cube", [-1.5, 0.0, occluder_z], [1.5, 3.0, 1.0]),
        ],
        "coverage",
    );
    let runtime = scene.runtime();
    let mut gpu = Gpu::new([128, 128], &runtime, Some(UiRenderOptions::default()));
    gpu.render(&runtime)
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_world_panel_is_occluded_by_geometry_in_front_and_visible_in_front_of_it() {
    let in_front = occlusion_image(1.0);
    in_front.save("world_panel_occluded");
    // Left of the centre the opaque quad is nearer: red. Right of it: the panel.
    assert_eq!(
        in_front.at(40, 64),
        RED,
        "panel behind the quad is occluded"
    );
    assert_eq!(
        in_front.at(88, 64),
        GREEN,
        "the uncovered half shows the panel"
    );
    let behind = occlusion_image(-1.0);
    behind.save("world_panel_visible");
    assert_eq!(
        behind.at(40, 64),
        GREEN,
        "panel in front of the quad is visible"
    );
    assert_eq!(behind.at(88, 64), GREEN);
    // Outside the panel the quad is still visible where it extends past it.
    assert_eq!(behind.at(40, 10), RED);
    let covered = in_front.count_near([0, 255, 0], 2);
    let visible = behind.count_near([0, 255, 0], 2);
    println!("panel pixels: occluded {covered}, unoccluded {visible}");
    assert!(visible > covered * 3 / 2, "{visible} vs {covered}");
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn world_panels_sort_back_to_front_regardless_of_entity_order() {
    for (near_color, far_color) in [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ] {
        let scene = Scene::new(
            vec![
                camera([128, 128], json!({})),
                panel(
                    "aaa-far",
                    [0.0, 0.0, -0.5],
                    [0.0, 0.0, 0.0, 1.0],
                    far_color,
                    json!({}),
                ),
                panel(
                    "zzz-near",
                    [0.0, 0.0, 0.5],
                    [0.0, 0.0, 0.0, 1.0],
                    near_color,
                    json!({}),
                ),
            ],
            "coverage",
        );
        let runtime = scene.runtime();
        let mut gpu = Gpu::new([128, 128], &runtime, Some(UiRenderOptions::default()));
        let image = gpu.render(&runtime);
        let expected = [
            (near_color[0] * 255.0) as u8,
            (near_color[1] * 255.0) as u8,
            0,
            255,
        ];
        assert_eq!(image.at(64, 64), expected, "the nearer panel wins");
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn a_billboard_panel_faces_the_camera_and_an_edge_on_one_does_not() {
    // Rotated 80 degrees about Y the panel is nearly edge-on to the camera.
    let rotation = [0.0, 0.642_787_6, 0.0, 0.766_044_4];
    let widths: Vec<usize> = ["none", "full"]
        .into_iter()
        .map(|billboard| {
            let scene = Scene::new(
                vec![
                    camera([128, 128], json!({})),
                    panel(
                        "panel",
                        [0.0, 0.0, 0.0],
                        rotation,
                        [0.0, 1.0, 0.0],
                        json!({"billboard":billboard}),
                    ),
                ],
                "coverage",
            );
            let runtime = scene.runtime();
            let mut gpu = Gpu::new([128, 128], &runtime, Some(UiRenderOptions::default()));
            let image = gpu.render(&runtime);
            image.save(&format!("billboard_{billboard}"));
            let row: Vec<[u8; 4]> = (0..128).map(|x| image.at(x, 64)).collect();
            row.iter().filter(|p| **p == GREEN).count()
        })
        .collect();
    println!(
        "green pixels on the middle row: edge-on {}, billboard {}",
        widths[0], widths[1]
    );
    assert!(widths[0] < 40, "edge-on panel is thin: {}", widths[0]);
    assert!(
        widths[1] > 90,
        "billboard panel faces the camera: {}",
        widths[1]
    );
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn world_text_draws_depth_tested_sdf_glyphs_in_3d() {
    let sign = |z: f32| {
        json!({"id":"sign","name":"Sign","components":{
            "Transform":{"position":[0.0, 0.0, z]},
            "custom":{"engine.WorldText":{"version":1,"data":{
                "text":"HELLO","font":"ui","size":1.2,"atlas":"sdf","billboard":true}}}}})
    };
    let mut counts = vec![];
    for (name, occluder_z) in [("clear", -3.0), ("blocked", 1.0)] {
        let scene = Scene::new(
            vec![
                camera([128, 128], json!({})),
                sign(0.0),
                occluder("wall", [0.0, 0.0, occluder_z], [3.0, 3.0, 1.0]),
            ],
            "sdf",
        );
        let runtime = scene.runtime();
        let mut gpu = Gpu::new([128, 128], &runtime, Some(UiRenderOptions::default()));
        let image = gpu.render(&runtime);
        image.save(&format!("world_text_{name}"));
        let status = runtime.text_engine().unwrap().render_status();
        assert_eq!(status.world.state, UiFeatureState::Gpu, "{status:?}");
        // White glyph pixels (the wall is red).
        counts.push(image.count_near([255, 255, 255], 40));
    }
    println!(
        "white glyph pixels: clear {}, blocked {}",
        counts[0], counts[1]
    );
    assert!(counts[0] > 150, "text is drawn: {}", counts[0]);
    assert!(
        counts[1] < 5,
        "a wall in front hides the text: {}",
        counts[1]
    );
}

// ---- (c) colour-vision filter -------------------------------------------------------

fn patches(filter: Value) -> Scene {
    let mut data = filter.as_object().unwrap().clone();
    data.insert("version".into(), json!(1));
    let access = json!({"id":"access","name":"Access","components":{"custom":{
        "engine.AccessibilitySettings":{"version":1,"data":data}}}});
    let patch = |id: &str, x: f32, tint: [f32; 3]| {
        json!({"id":id,"name":id,"components":{
            "Transform":{"position":[x, 0.0, 0.0],"scale":[1.0, 1.0, 1.0]},
            "MeshRenderer":{"mesh":"quad"},
            "MeshMaterial":{"tint":[tint[0],tint[1],tint[2],1],"unlit":true}}})
    };
    Scene::new(
        vec![
            camera([128, 128], json!({})),
            access,
            patch("red", -1.5, [1.0, 0.0, 0.0]),
            patch("green", 1.5, [0.0, 1.0, 0.0]),
        ],
        "coverage",
    )
}

fn encode(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|c| (linear_to_srgb(c) * 255.0).round() as u8)
}

fn filtered_patches(settings: Value, expected: ColorFilterSettings) -> (Image, UiRenderStatus) {
    let scene = patches(settings);
    let mut runtime = scene.runtime();
    let mut gpu = Gpu::new([128, 128], &runtime, Some(UiRenderOptions::default()));
    // The component reaches the settings resource on a UI tick.
    runtime.tick(0.016, Default::default()).unwrap();
    let image = gpu.render(&runtime);
    let queue = runtime.app().world().resource::<UiGpuQueue>().clone();
    assert_eq!(queue.frame().color_filter, expected);
    (image, queue.status())
}

fn assert_patch(image: &Image, x: u32, linear_in: [f32; 3], expected: ColorFilterSettings) {
    let want = encode(apply_linear(linear_in, expected));
    let got = image.at(x, 64);
    for channel in 0..3 {
        assert!(
            got[channel].abs_diff(want[channel]) <= 3,
            "patch at x={x}: got {got:?}, expected {want:?} for {expected:?}"
        );
    }
}

#[test]
#[ignore = "requires a native GPU adapter"]
fn the_colour_filter_turns_red_and_green_patches_into_the_simulated_colours() {
    // Baseline: the filter is registered but off.
    let off = ColorFilterSettings::default();
    let (image, _) = filtered_patches(json!({}), off);
    assert_eq!(image.at(32, 64), RED);
    assert_eq!(image.at(96, 64), GREEN);
    image.save("filter_off");
    for (name, kind, mode, strength) in [
        (
            "protanopia",
            ColorFilterKind::Protanopia,
            ColorFilterMode::Simulate,
            1.0,
        ),
        (
            "deuteranopia",
            ColorFilterKind::Deuteranopia,
            ColorFilterMode::Simulate,
            1.0,
        ),
        (
            "tritanopia",
            ColorFilterKind::Tritanopia,
            ColorFilterMode::Simulate,
            1.0,
        ),
        (
            "achromatopsia",
            ColorFilterKind::Achromatopsia,
            ColorFilterMode::Simulate,
            1.0,
        ),
        (
            "deuteranopia_half",
            ColorFilterKind::Deuteranopia,
            ColorFilterMode::Simulate,
            0.5,
        ),
        (
            "deuteranopia_corrected",
            ColorFilterKind::Deuteranopia,
            ColorFilterMode::Correct,
            1.0,
        ),
    ] {
        let expected = ColorFilterSettings {
            kind,
            mode,
            strength,
        };
        let (image, status) = filtered_patches(
            json!({"colorFilter":kind.name(),"colorFilterMode":mode.name(),"colorFilterStrength":strength}),
            expected,
        );
        image.save(&format!("filter_{name}"));
        // Red patch on the left of the view, green on the right.
        assert_patch(&image, 32, [1.0, 0.0, 0.0], expected);
        assert_patch(&image, 96, [0.0, 1.0, 0.0], expected);
        let filter = status.feature(UiGpuFeature::ColorFilter).unwrap();
        assert_eq!(filter.state, UiFeatureState::Gpu, "{filter:?}");
        assert!(filter.frames >= 1, "{filter:?}");
        println!(
            "{name}: red -> {:?} green -> {:?}",
            image.at(32, 64),
            image.at(96, 64)
        );
    }
}
