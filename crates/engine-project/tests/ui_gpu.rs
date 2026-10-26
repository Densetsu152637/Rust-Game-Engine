//! The CPU side of the UI render features: SDF layouts, the routing rule that
//! keeps routed content out of the 2D draw list, the published frame, the
//! status reports and the colour-vision filter settings. The GPU side is in
//! `engine-ui-render`.
use engine_project::{
    ComponentCatalog, CustomValue, Project, Result, Runtime,
    accessibility::{
        AccessibilityControl, AccessibilitySettings, ColorFilterKind, ColorFilterMode,
    },
    content::{FontAtlasMode, FontFamilySettings},
    runtime::DrawSpace,
    ui_tree::*,
};
use engine_text::AtlasConfig;
use serde_json::{Value, json};
use std::fs;

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

fn label_root(width: u32) -> Value {
    json!({"version":1,"tree":{"nodes":[
        {"id":"label","widget":{"kind":"label","text":"Score 120"},
         "style":{"font":"ui","width":{"unit":"pixels","value":width},"height":{"unit":"pixels","value":30}}}
    ]}})
}

/// A scene with a HUD label, a world panel, world text and (optionally) a 3D camera.
fn scene(camera: Option<Value>) -> Value {
    let mut entities = vec![
        json!({"id":"hud","name":"HUD","components":{"custom":{"engine.UiRoot":{"version":1,"data":label_root(200)}}}}),
        json!({"id":"panel","name":"Panel","components":{
            "Transform":{"position":[0,1,0]},
            "custom":{
                "engine.UiRoot":{"version":1,"data":label_root(150)},
                "engine.UiWorldPanel":{"version":1,"data":{"size":[2,1],"pixelsPerUnit":100}}}}}),
        json!({"id":"sign","name":"Sign","components":{
            "Transform":{"position":[0,-1,0]},
            "custom":{"engine.WorldText":{"version":1,"data":{
                "text":"Exit","font":"ui","size":0.5,"atlas":"sdf","billboard":true}}}}}),
    ];
    if let Some(camera) = camera {
        entities.push(json!({"id":"camera","name":"Camera","components":{
            "Transform":{"position":[0,0,5]},"Camera3D":camera}}));
    }
    json!({"schema":1,"scene_id":"main","entities":entities})
}

fn runtime(scene: Value, sdf_family: bool) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ui.ttf"), UBUNTU).unwrap();
    fs::write(
        dir.path().join("fonts.json"),
        json!({"schema":1,"defaultFamily":"ui","families":{"ui":{
            "size":18,"atlas": if sdf_family {"sdf"} else {"coverage"}}}})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"UI GPU","startup_scene":"scene.json","assets":[
            {"id":"ui","kind":"font_face","path":"ui.ttf"},
            {"id":"fonts","kind":"font_config","path":"fonts.json"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(dir.path().join("scene.json"), scene.to_string()).unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}

fn queue(runtime: &Runtime) -> UiGpuQueue {
    runtime.app().world().resource::<UiGpuQueue>().clone()
}

/// Pretend the three features were registered and prepared on a renderer.
fn ready(queue: &UiGpuQueue) {
    for feature in UiGpuFeature::ALL {
        queue.mark_registered(feature);
        queue.set_ready(feature, true);
    }
}

fn ids(runtime: &Runtime) -> Vec<String> {
    runtime
        .extract_frame()
        .unwrap()
        .draws
        .iter()
        .map(|d| d.id.clone())
        .collect()
}

#[test]
fn without_the_features_everything_stays_on_the_2d_path() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    let frame = runtime.extract_frame().unwrap();
    let id = |prefix: &str| frame.draws.iter().any(|d| d.id.starts_with(prefix));
    assert!(id("worldtext:sign"), "world text draws as a 2D world mesh");
    assert!(id("world:game-ui:"), "the panel draws as a 2D world mesh");
    let screen_text: Vec<_> = frame
        .draws
        .iter()
        .filter(|d| d.space == DrawSpace::Ui && !is_sdf_draw(d))
        .collect();
    assert!(
        screen_text.iter().any(|d| d.texture.is_some()),
        "screen text uses a coverage page"
    );
    assert!(frame.draws.iter().all(|d| !is_sdf_draw(d)));
    assert!(
        queue(&runtime).frame().world.is_empty(),
        "nothing is published while no feature is registered"
    );
}

#[test]
fn ready_features_take_their_content_out_of_the_2d_draw_list() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    let queue = queue(&runtime);
    ready(&queue);
    let frame = runtime.extract_frame().unwrap();
    assert!(
        frame
            .draws
            .iter()
            .all(|d| !d.id.starts_with("worldtext:") && !d.id.starts_with("world:")),
        "world UI is not drawn twice: {:?}",
        frame.draws.iter().map(|d| &d.id).collect::<Vec<_>>()
    );
    assert!(
        frame.draws.iter().all(|d| !is_sdf_draw(d)),
        "SDF text is not in the sprite draw list"
    );
    let published = queue.frame();
    assert_eq!(published.ui_size, [320, 180]);
    assert!(
        !published.sdf_text.is_empty() && published.sdf_text.iter().all(is_sdf_draw),
        "the HUD label is an SDF run"
    );
    assert!(
        published.sdf_text.iter().all(|d| d.space == DrawSpace::Ui),
        "SDF runs keep UI-space coordinates"
    );
    let kinds: Vec<(String, WorldUiKind)> = published
        .world
        .iter()
        .map(|i| (i.id.clone(), i.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("worldtext:sign".to_owned(), WorldUiKind::Text),
            ("world:panel".to_owned(), WorldUiKind::Panel),
        ]
    );
    let sign = &published.world[0];
    assert_eq!(sign.billboard, WorldBillboard::Screen);
    assert!(sign.draws.iter().all(|d| d.sdf), "atlas sdf uses SDF pages");
    let panel = &published.world[1];
    assert_eq!(panel.billboard, WorldBillboard::None);
    assert!(panel.draws.iter().all(|d| !d.sdf), "panel text is coverage");
}

#[test]
fn world_ui_keeps_the_2d_path_without_a_3d_camera_or_when_disabled() {
    // No Camera3D: today's 2D behaviour even though the feature is ready.
    let (_dir, runtime2d) = runtime(scene(None), true);
    ready(&queue(&runtime2d));
    let list = ids(&runtime2d);
    assert!(list.iter().any(|i| i.starts_with("worldtext:sign")));
    assert!(list.iter().any(|i| i.starts_with("world:game-ui:")));
    assert!(queue(&runtime2d).frame().world.is_empty());
    // The screen SDF text is independent of the camera kind.
    assert!(!queue(&runtime2d).frame().sdf_text.is_empty());

    // A per-camera override puts the content back on the 2D path.
    let (_dir, runtime3d) = runtime(
        scene(Some(
            json!({"render_features":{"ui.world":false,"ui.sdf_text":false}}),
        )),
        true,
    );
    ready(&queue(&runtime3d));
    let list = ids(&runtime3d);
    assert!(list.iter().any(|i| i.starts_with("worldtext:sign")));
    assert!(list.iter().any(|i| i.starts_with("world:game-ui:")));
    assert!(queue(&runtime3d).frame().world.is_empty());
    assert!(queue(&runtime3d).frame().sdf_text.is_empty());
    let (state, reason) = queue(&runtime3d).state_of(UiGpuFeature::World);
    assert_eq!(state, UiFeatureState::Fallback);
    assert!(reason.unwrap().contains("render_features"));
}

#[test]
fn coverage_families_never_produce_sdf_runs() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), false);
    ready(&queue(&runtime));
    let frame = runtime.extract_frame().unwrap();
    assert!(frame.draws.iter().all(|d| !is_sdf_draw(d)));
    assert!(queue(&runtime).frame().sdf_text.is_empty());
}

#[test]
fn world_text_geometry_is_anchored_y_up_and_sized_in_world_units() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    ready(&queue(&runtime));
    runtime.extract_frame().unwrap();
    let published = queue(&runtime).frame();
    let sign = published
        .world
        .iter()
        .find(|i| i.kind == WorldUiKind::Text)
        .unwrap();
    let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
    for draw in &sign.draws {
        for v in &draw.mesh.vertices {
            assert_eq!(v.position[2], 0.0);
            for axis in 0..2 {
                min[axis] = min[axis].min(v.position[axis]);
                max[axis] = max[axis].max(v.position[axis]);
            }
        }
    }
    // Anchor [0.5, 0.5]: the text box is centred on the origin; size 0.5 world
    // units means one em, so the glyph box is at most about half a unit tall
    // (SDF quads also carry the 6 px spread of the 48 px base).
    let centre = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    assert!(
        centre[0].abs() < 0.15 && centre[1].abs() < 0.25,
        "{centre:?}"
    );
    assert!(
        max[1] - min[1] < 0.9 && max[0] - min[0] > 0.2,
        "{min:?} {max:?}"
    );
    // y up: the glyph 'E' ascends above the baseline, so the box spans both signs.
    assert!(min[1] < 0.0 && max[1] > 0.0);
    let world = glam::Mat4::from_cols_array_2d(&sign.world_from_local);
    assert!((world.w_axis.truncate() - glam::Vec3::new(0.0, -1.0, 0.0)).length() < 1e-5);
}

#[test]
fn panel_items_are_panel_local_and_keep_draw_order() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    ready(&queue(&runtime));
    runtime.extract_frame().unwrap();
    let published = queue(&runtime).frame();
    let panel = published
        .world
        .iter()
        .find(|i| i.kind == WorldUiKind::Panel)
        .unwrap();
    assert!(!panel.draws.is_empty());
    for draw in &panel.draws {
        for v in &draw.mesh.vertices {
            // 2 x 1 panel, pivot centre: x in [-1, 1], y in [-0.5, 0.5], flat.
            assert!(v.position[0].abs() <= 1.0 + 1e-4 && v.position[1].abs() <= 0.5 + 1e-4);
            assert_eq!(v.position[2], 0.0);
        }
    }
}

#[test]
fn sdf_layouts_use_one_bitmap_for_every_size_and_scale_the_quads() {
    let mut engine = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    engine
        .register_font(
            UBUNTU.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(16.0),
                atlas: FontAtlasMode::Sdf,
            },
        )
        .unwrap();
    let request = |scale: f32| UiTextRequest {
        text: "Ag",
        kind: UiTextKind::Plain,
        font: "ui",
        scale,
        color: [1.0; 4],
        max_width: None,
    };
    // Without SDF output the family renders through a coverage page.
    let coverage = engine.layout(&request(1.0)).unwrap();
    assert!(!coverage.sdf);
    engine.set_sdf_output(true);
    let small = engine.layout(&request(1.0)).unwrap();
    let large = engine.layout(&request(2.0)).unwrap();
    assert!(small.sdf && large.sdf);
    assert_eq!(small.glyphs.len(), 2);
    for (a, b) in small.glyphs.iter().zip(&large.glyphs) {
        assert_eq!(a.uv, b.uv, "one 48 px bitmap serves every size");
        let ratio = b.rect.height / a.rect.height;
        assert!(
            (ratio - 2.0).abs() < 0.02,
            "quads scale with the size: {ratio}"
        );
    }
    // The quad of 'A' at 16 px covers the 48 px glyph plus the 6 px spread
    // scaled by 16/48, so it is larger than the 16 px coverage bitmap.
    assert!(small.glyphs[0].rect.height > coverage.glyphs[0].rect.height);
    // SDF pages hold a distance gradient (alpha values between 0 and 255).
    let page = &small.pages[small.glyphs[0].page as usize];
    let partial = page
        .pixels
        .chunks(4)
        .filter(|p| p[3] > 0 && p[3] < 255)
        .count();
    assert!(partial > 500, "{partial} gradient texels");
    // SDF and coverage pages are different textures.
    assert!(!std::sync::Arc::ptr_eq(&page.clone(), &coverage.pages[0]));
    engine.set_sdf_output(false);
    assert!(!engine.layout(&request(1.0)).unwrap().sdf);
    assert!(
        engine.stats().atlas_pages >= 2,
        "a coverage and an SDF atlas"
    );
}

#[test]
fn sdf_output_only_affects_families_that_request_sdf() {
    let mut engine = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    engine
        .register_font(UBUNTU.to_vec(), "plain", FontFamilySettings::default())
        .unwrap();
    engine.set_sdf_output(true);
    let laid = engine
        .layout(&UiTextRequest {
            text: "Hi",
            kind: UiTextKind::Plain,
            font: "plain",
            scale: 1.0,
            color: [1.0; 4],
            max_width: None,
        })
        .unwrap();
    assert!(!laid.sdf);
}

#[test]
fn text_render_status_tracks_registration_and_readiness() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    let queue = queue(&runtime);
    let engine = runtime.text_engine().unwrap().clone();
    // 1. nothing registered
    let status = engine.render_status();
    assert_eq!(status.state, TextRenderState::SdfFallback);
    assert_eq!(status.screen.state, UiFeatureState::Fallback);
    assert!(
        status
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("not registered")
    );
    // 2. registered, not yet prepared by a renderer
    for feature in UiGpuFeature::ALL {
        queue.mark_registered(feature);
    }
    let status = engine.render_status();
    assert!(
        status
            .screen
            .reason
            .as_deref()
            .unwrap()
            .contains("no wgpu renderer has prepared it")
    );
    // 3. prepared: SDF is active once a frame has used it
    for feature in UiGpuFeature::ALL {
        queue.set_ready(feature, true);
    }
    runtime.extract_frame().unwrap();
    let status = engine.render_status();
    assert_eq!(status.requested, FontAtlasMode::Sdf);
    assert_eq!(status.state, TextRenderState::Sdf, "{status:?}");
    assert!(status.fallback_reason.is_none());
    assert!(status.sdf_gpu_layouts >= 2, "HUD label and world text");
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["state"], "sdf");
    assert_eq!(json["screen"]["state"], "gpu");
    // 4. unprepared again (device lost): back to the coverage fallback
    queue.set_ready(UiGpuFeature::SdfText, false);
    assert_eq!(engine.render_status().state, TextRenderState::SdfFallback);
}

#[test]
fn world_text_sdf_without_a_3d_camera_reports_the_world_path_as_fallback() {
    let (_dir, runtime) = runtime(scene(None), true);
    let queue = queue(&runtime);
    ready(&queue);
    runtime.extract_frame().unwrap();
    let status = runtime.text_engine().unwrap().render_status();
    assert_eq!(status.state, TextRenderState::SdfFallback);
    assert_eq!(status.screen.state, UiFeatureState::Gpu);
    assert_eq!(status.world.state, UiFeatureState::Fallback);
    assert!(
        status
            .fallback_reason
            .unwrap()
            .contains("no active Camera3D")
    );
}

#[test]
fn the_runtime_snapshot_reports_every_ui_feature() {
    let (_dir, runtime) = runtime(scene(Some(json!({}))), true);
    let snapshot = runtime.snapshot();
    let status = snapshot.ui_render.expect("always present");
    assert_eq!(status.features.len(), 3);
    assert!(
        status
            .features
            .iter()
            .all(|f| f.state == UiFeatureState::Fallback && f.reason.is_some())
    );
    ready(&queue(&runtime));
    let status = runtime.snapshot().ui_render.unwrap();
    assert!(
        status
            .features
            .iter()
            .all(|f| f.state == UiFeatureState::Gpu)
    );
    let json = serde_json::to_value(runtime.snapshot()).unwrap();
    assert_eq!(json["ui_render"]["features"][0]["name"], "ui.sdf_text");
}

// ---- colour-vision filter settings -------------------------------------------------

fn validate(data: Value) -> Result<()> {
    ComponentCatalog::new(vec![]).unwrap().validate(
        "engine.AccessibilitySettings",
        &CustomValue { version: 1, data },
    )
}

#[test]
fn color_filter_settings_are_validated_through_the_descriptor() {
    for good in [
        json!({"version":1,"colorFilter":"protanopia"}),
        json!({"version":1,"colorFilter":"deuteranopia","colorFilterMode":"correct","colorFilterStrength":0.5}),
        json!({"version":1,"colorFilter":"tritanopia","colorFilterMode":"correct"}),
        json!({"version":1,"colorFilter":"achromatopsia","colorFilterMode":"simulate","colorFilterStrength":1}),
        json!({"version":1,"colorFilter":"none","colorFilterStrength":0}),
    ] {
        validate(good.clone()).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        json!({"version":1,"colorFilter":"red-green"}),
        json!({"version":1,"colorFilter":1}),
        json!({"version":1,"colorFilterMode":"fix"}),
        json!({"version":1,"colorFilterStrength":1.5}),
        json!({"version":1,"colorFilterStrength":-0.1}),
        json!({"version":1,"colorFilter":"achromatopsia","colorFilterMode":"correct"}),
    ] {
        assert!(validate(bad.clone()).is_err(), "{bad} should be rejected");
    }
    let defaults = AccessibilitySettings::default();
    assert_eq!(defaults.color_filter, ColorFilterKind::None);
    assert!(!defaults.color_filter_settings().is_active());
}

#[test]
fn the_native_api_validates_and_the_filter_reaches_the_published_frame() {
    let (_dir, mut runtime) = runtime(scene(Some(json!({}))), true);
    let queue = queue(&runtime);
    queue.mark_registered(UiGpuFeature::ColorFilter);
    queue.set_ready(UiGpuFeature::ColorFilter, true);
    {
        let world = runtime.app_mut().world_mut();
        let mut control = world.resource_mut::<AccessibilityControl>();
        assert!(
            control
                .set_color_filter(
                    ColorFilterKind::Achromatopsia,
                    ColorFilterMode::Correct,
                    1.0
                )
                .is_err()
        );
        assert!(
            control
                .set_color_filter(ColorFilterKind::Protanopia, ColorFilterMode::Simulate, 2.0)
                .is_err()
        );
        control
            .set_color_filter(
                ColorFilterKind::Deuteranopia,
                ColorFilterMode::Correct,
                0.75,
            )
            .unwrap();
    }
    runtime.extract_frame().unwrap();
    let filter = queue.frame().color_filter;
    assert_eq!(
        (filter.kind, filter.mode, filter.strength),
        (
            ColorFilterKind::Deuteranopia,
            ColorFilterMode::Correct,
            0.75
        )
    );
    assert!(filter.is_active());
}

#[test]
fn the_authored_component_drives_the_filter_after_a_tick() {
    let mut entities = scene(Some(json!({})));
    entities["entities"].as_array_mut().unwrap().push(json!({
        "id":"access","name":"Access","components":{"custom":{"engine.AccessibilitySettings":{
            "version":1,"data":{"version":1,"colorFilter":"tritanopia","colorFilterStrength":0.6}}}}
    }));
    let (_dir, mut runtime) = runtime(entities, true);
    let queue = queue(&runtime);
    queue.mark_registered(UiGpuFeature::ColorFilter);
    runtime.tick(0.016, Default::default()).unwrap();
    runtime.extract_frame().unwrap();
    let filter = queue.frame().color_filter;
    assert_eq!(filter.kind, ColorFilterKind::Tritanopia);
    assert!((filter.strength - 0.6).abs() < 1e-6);
}

#[test]
fn open_dialogs_keep_sdf_text_routed_instead_of_switching_the_root_to_coverage() {
    let mut document = scene(None);
    let label = UiNode::label("behind", "Behind")
        .with_size(UiLength::Pixels(200.0), UiLength::Pixels(30.0))
        .with_style(|s| s.font = "ui".into());
    let mut dialog = UiNode::dialog("dialog", "Dialog", vec!["Continue".into()])
        .with_size(UiLength::Pixels(200.0), UiLength::Pixels(100.0))
        .with_style(|s| s.font = "ui".into());
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let root = UiRoot {
        tree: UiTree {
            nodes: vec![label, dialog],
        },
        ..Default::default()
    };
    document["entities"][0]["components"]["custom"]["engine.UiRoot"]["data"] =
        serde_json::to_value(root).unwrap();
    let (_dir, runtime) = runtime(document, true);
    let queue = queue(&runtime);
    ready(&queue);
    let frame = runtime.extract_frame().unwrap();
    let published = queue.frame();
    assert!(published.sdf_text.iter().any(|draw| draw.layer == 0));
    assert!(
        published
            .sdf_text
            .iter()
            .any(|draw| draw.layer == UI_DIALOG_LAYER)
    );
    assert!(frame.draws.iter().all(|draw| !is_sdf_draw(draw)));
}

#[test]
fn final_filter_status_supports_2d_and_reports_camera_disable_precisely() {
    let queue = UiGpuQueue::default();
    queue.mark_registered(UiGpuFeature::ColorFilter);
    queue.set_ready(UiGpuFeature::ColorFilter, true);
    queue.route(None);
    queue.publish(UiGpuFrame {
        color_filter: engine_project::accessibility::ColorFilterSettings {
            kind: ColorFilterKind::Protanopia,
            mode: ColorFilterMode::Simulate,
            strength: 1.0,
        },
        ..Default::default()
    });
    assert!(
        queue
            .state_of(UiGpuFeature::ColorFilter)
            .1
            .unwrap()
            .contains("no final presentation")
    );
    queue.report(
        UiGpuFeature::ColorFilter,
        UiFeatureReport { draws: 1, items: 1 },
    );
    assert_eq!(
        queue.state_of(UiGpuFeature::ColorFilter),
        (UiFeatureState::Gpu, None)
    );
    queue.route(Some(&std::collections::BTreeMap::from([(
        FEATURE_COLOR_FILTER.to_owned(),
        false,
    )])));
    assert!(
        queue
            .state_of(UiGpuFeature::ColorFilter)
            .1
            .unwrap()
            .contains("disabled for the active camera")
    );
}
