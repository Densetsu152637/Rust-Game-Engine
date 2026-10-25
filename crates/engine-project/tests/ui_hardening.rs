#![recursion_limit = "512"]
//! Regression tests for player-reachable UI failures: text input past the
//! string byte limit, rich-label markup that the text engine rejects, text
//! scale pushing the pixel size out of range, unvalidated accessibility
//! settings, and world panels missing screen-root accessibility settings.
//!
//! The desktop runner exits when a tick or extraction returns an error, so each
//! of these used to be a crash.
use bevy_ecs::prelude::World;
use engine_project::{
    ActionFrame, Project, Runtime,
    accessibility::{AccessibilityControl, AccessibilitySettings, UiScaling},
    content::{FontAtlas, FontAtlasMode, FontFamilySettings, FontGlyph, Texture},
    ui_tree::*,
};
use engine_text::AtlasConfig;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, sync::Arc};

const UBUNTU: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

fn px(v: f32) -> UiLength {
    UiLength::Pixels(v)
}
fn at(node: UiNode, x: f32, y: f32, w: f32, h: f32) -> UiNode {
    node.with_size(px(w), px(h)).with_style(|s| {
        s.offset = [x, y];
        s.padding = 0.0;
        s.gap = 0.0;
    })
}
fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}
fn press(action: &str, pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pressed: [action.into()].into(),
        pointer,
        ..Default::default()
    }
}
fn idle() -> ActionFrame {
    ActionFrame::default()
}

// ---- GameUi on the bitmap backend ------------------------------------------

fn bitmap_assets() -> (BTreeMap<String, FontAtlas>, BTreeMap<String, Arc<Texture>>) {
    (
        BTreeMap::from([(
            "font".into(),
            FontAtlas {
                texture: "atlas".into(),
                line_height: 8.0,
                fallback: 65,
                glyphs: BTreeMap::from([(
                    65,
                    FontGlyph {
                        source: [0, 0, 4, 8],
                        advance: 5.0,
                        bearing: [0.0; 2],
                    },
                )]),
            },
        )]),
        BTreeMap::from([(
            "atlas".into(),
            Arc::new(Texture {
                width: 4,
                height: 8,
                pixels: vec![255; 128],
            }),
        )]),
    )
}

fn text_input(max_length: u32) -> UiNode {
    let mut node = at(
        UiNode::text_input("name", "", max_length),
        0.0,
        0.0,
        200.0,
        40.0,
    )
    .with_action("name");
    node.style.font = "font".into();
    node
}

fn input_of(root: &UiRoot) -> String {
    match &root.tree.nodes[root.tree.find("name").unwrap()].widget {
        UiWidget::TextInput { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    }
}

struct Bitmap {
    fonts: BTreeMap<String, FontAtlas>,
    textures: BTreeMap<String, Arc<Texture>>,
}

impl Bitmap {
    fn new() -> Self {
        let (fonts, textures) = bitmap_assets();
        Self { fonts, textures }
    }

    fn tick(
        &self,
        ui: &mut GameUi,
        roots: &mut [(String, UiRoot)],
        input: &ActionFrame,
        events: Vec<UiTextEvent>,
    ) -> engine_project::Result<Vec<UiWidgetEvent>> {
        let backend = BitmapTextBackend::new(&self.fonts, &self.textures);
        let ctx = UiTickContext {
            viewport: [400, 300],
            dt: 0.0,
            text: &backend,
        };
        for event in events {
            ui.push_text_event(event).unwrap();
        }
        ui.tick_with(roots, input, &ctx)
    }
}

#[test]
fn typed_and_pasted_multibyte_text_stops_at_the_string_byte_limit() {
    let bitmap = Bitmap::new();
    let mut roots = [("hud".to_string(), root(vec![text_input(4096)]))];
    let mut ui = GameUi::default();
    bitmap
        .tick(&mut ui, &mut roots, &press("ui_click", [5.0, 5.0]), vec![])
        .unwrap();
    // 1705 three-byte characters are 5115 bytes; `maxLength` alone (4096
    // characters) would admit them all and the next tick would fail validation.
    let inserts = (0..5)
        .map(|_| UiTextEvent::Insert("あ".repeat(341)))
        .collect();
    bitmap.tick(&mut ui, &mut roots, &idle(), inserts).unwrap();
    let value = input_of(&roots[0].1);
    assert_eq!(
        value.len(),
        4095,
        "fills the limit without splitting a char"
    );
    assert_eq!(value.chars().count(), 1365);
    bitmap
        .tick(&mut ui, &mut roots, &idle(), vec![])
        .expect("the next tick validates the edited root");

    // Paste at the limit adds nothing; with room for one character it adds one.
    let key = |k| UiTextEvent::key(k);
    bitmap
        .tick(
            &mut ui,
            &mut roots,
            &idle(),
            vec![
                key(UiEditKey::SelectAll),
                key(UiEditKey::Copy),
                key(UiEditKey::Right),
                key(UiEditKey::Paste),
            ],
        )
        .unwrap();
    assert_eq!(input_of(&roots[0].1).len(), 4095);
    bitmap
        .tick(
            &mut ui,
            &mut roots,
            &idle(),
            vec![key(UiEditKey::Backspace), key(UiEditKey::Paste)],
        )
        .unwrap();
    assert_eq!(input_of(&roots[0].1).len(), 4095, "one character refilled");
    bitmap
        .tick(&mut ui, &mut roots, &idle(), vec![])
        .expect("still valid");
}

fn full_root_with_input() -> UiRoot {
    // 15 x 4096 + 3000 bytes of label text leave 1096 of the root's 64 KiB.
    let mut nodes: Vec<UiNode> = (0..15)
        .map(|i| {
            let mut label = at(
                UiNode::label(format!("l{i}"), "a".repeat(4096)),
                0.0,
                50.0,
                10.0,
                10.0,
            );
            label.style.font = "font".into();
            label
        })
        .collect();
    let mut tail = at(
        UiNode::label("tail", "a".repeat(3000)),
        0.0,
        50.0,
        10.0,
        10.0,
    );
    tail.style.font = "font".into();
    nodes.push(tail);
    nodes.push(text_input(4096));
    root(nodes)
}

#[test]
fn typing_respects_what_is_left_of_the_root_text_budget() {
    let bitmap = Bitmap::new();
    let mut roots = [("hud".to_string(), full_root_with_input())];
    roots[0].1.validate().unwrap();
    let mut ui = GameUi::default();
    bitmap
        .tick(&mut ui, &mut roots, &press("ui_click", [5.0, 5.0]), vec![])
        .unwrap();
    bitmap
        .tick(
            &mut ui,
            &mut roots,
            &idle(),
            vec![
                UiTextEvent::Insert("a".repeat(1000)),
                UiTextEvent::Insert("a".repeat(1000)),
            ],
        )
        .unwrap();
    assert_eq!(input_of(&roots[0].1).len(), 1096);
    roots[0].1.validate().expect("root stays within 64 KiB");
    bitmap
        .tick(&mut ui, &mut roots, &idle(), vec![])
        .expect("next tick");
}

#[test]
fn assistive_technology_set_text_respects_the_root_text_budget() {
    let bitmap = Bitmap::new();
    let mut roots = [("hud".to_string(), full_root_with_input())];
    let mut ui = GameUi::default();
    ui.set_accessibility(true);
    let set = |ui: &mut GameUi, text: String| {
        ui.request_access(UiAccessRequest {
            target: AccessId::node("hud", "name"),
            action: UiAccessAction::SetText(text),
        })
        .unwrap();
    };
    set(&mut ui, "a".repeat(1097));
    set(&mut ui, "a".repeat(1096));
    bitmap.tick(&mut ui, &mut roots, &idle(), vec![]).unwrap();
    let status: Vec<_> = ui
        .take_access_outcomes()
        .into_iter()
        .map(|o| o.status)
        .collect();
    assert_eq!(status, [UiAccessStatus::Rejected, UiAccessStatus::Applied]);
    assert_eq!(input_of(&roots[0].1).len(), 1096);
    bitmap.tick(&mut ui, &mut roots, &idle(), vec![]).unwrap();
}

// ---- Runtime with a TrueType font -------------------------------------------

fn runtime_with(
    entities: Vec<Value>,
    extra_assets: Vec<Value>,
    files: &[(&str, Value)],
) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ui.ttf"), UBUNTU).unwrap();
    for (name, content) in files {
        fs::write(dir.path().join(name), content.to_string()).unwrap();
    }
    let mut assets = vec![json!({"id":"ui","kind":"font_face","path":"ui.ttf"})];
    assets.extend(extra_assets);
    fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":entities}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"hardening","startup_scene":"scene.json","assets":assets})
            .to_string(),
    )
    .unwrap();
    let project = Project::load(dir.path().join("project.json")).unwrap();
    project.validate().unwrap();
    let mut runtime = Runtime::new(project).unwrap();
    runtime.start().unwrap();
    (dir, runtime)
}

fn ui_entity(id: &str, root: &UiRoot) -> Value {
    json!({"id":id,"name":id,"components":{"custom":{
        "engine.UiRoot":{"version":1,"data":root}}}})
}

fn ttf(node: UiNode) -> UiNode {
    node.with_style(|s| {
        s.font = "ui".into();
        s.padding = 0.0;
    })
}

fn tick(runtime: &mut Runtime) {
    runtime
        .tick(1.0 / 60.0, ActionFrame::default())
        .expect("tick");
}

fn data(runtime: &mut Runtime) -> bevy_ecs::prelude::Mut<'_, UiDataContext> {
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
}

fn stored_root(runtime: &Runtime, id: &str) -> UiRoot {
    let snapshot = runtime.try_snapshot().unwrap();
    let entity = snapshot.scene.entities.iter().find(|e| e.id == id).unwrap();
    serde_json::from_value(entity.components.custom["engine.UiRoot"].data.clone()).unwrap()
}

#[test]
fn bound_text_on_a_text_input_stays_within_the_byte_limit() {
    let mut tree = root(vec![at(
        ttf(UiNode::text_input("name", "", 4096)),
        0.0,
        0.0,
        300.0,
        60.0,
    )]);
    tree.bindings = vec![UiBinding::one_way(
        "name",
        UiBindProperty::Text,
        "player.name",
    )];
    let (_dir, mut runtime) = runtime_with(vec![ui_entity("hud", &tree)], vec![], &[]);
    data(&mut runtime)
        .set("player.name", json!("あ".repeat(2000)))
        .unwrap();
    tick(&mut runtime);
    tick(&mut runtime);
    runtime.extract_frame().expect("extract");
    let stored = stored_root(&runtime, "hud");
    let value = input_of(&stored);
    assert_eq!(value.len(), 4095);
    stored.validate().expect("the bound root validates");
}

#[test]
fn authored_markup_is_validated_and_run_time_markup_is_shown_literally() {
    for bad in ["[b]open", "[size=9999]big[/size]"] {
        let tree = root(vec![UiNode::rich_label("rich", bad)]);
        let error = tree.validate().unwrap_err();
        assert!(
            error.to_string().contains("RichLabel markup invalid"),
            "{bad}: {error}"
        );
    }
    root(vec![UiNode::rich_label("rich", "[b]fine[/b] \\[x\\]")])
        .validate()
        .expect("well formed markup");

    let mut tree = root(vec![at(
        ttf(UiNode::rich_label("rich", "")),
        0.0,
        0.0,
        300.0,
        60.0,
    )]);
    tree.bindings = vec![UiBinding::one_way("rich", UiBindProperty::Text, "message")];
    let (_dir, mut runtime) = runtime_with(vec![ui_entity("hud", &tree)], vec![], &[]);
    // Malformed known tag, bad tag value, and a valid span cut mid-tag by the
    // 4096-byte slot: each is bound data nobody validated.
    let cut_mid_tag = format!("[b]{}[/b]", "x".repeat(4094));
    for message in ["[b]x", "[size=9999]x", cut_mid_tag.as_str(), "ok [i]ok[/i]"] {
        data(&mut runtime).set("message", json!(message)).unwrap();
        tick(&mut runtime);
        runtime
            .extract_frame()
            .unwrap_or_else(|e| panic!("{message:.16}: {e}"));
        stored_root(&runtime, "hud")
            .validate()
            .unwrap_or_else(|e| panic!("{message:.16}: {e}"));
    }
}

#[test]
fn text_scale_cannot_push_the_pixel_size_out_of_range() {
    let mut backend = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    backend
        .register_font(
            UBUNTU.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(48.0),
                atlas: FontAtlasMode::Coverage,
            },
        )
        .unwrap();
    let request = |scale: f32| UiTextRequest {
        text: "Scale",
        kind: UiTextKind::Plain,
        font: "ui",
        scale,
        color: [1.0; 4],
        max_width: None,
    };
    // textScale 4 x fontScale 3 on a 48 px family is 576 px.
    let big = backend.measure(&request(12.0)).expect("measure at 576 px");
    backend.layout(&request(12.0)).expect("layout at 576 px");
    let edge = backend.measure(&request(512.0 / 48.0)).unwrap();
    assert_eq!(big.width, edge.width, "clamped to the 512 px limit");
    assert_eq!(backend.pixel_size("ui", 12.0), Some(512.0));
    backend.measure(&request(0.001)).expect("clamped to 1 px");
    assert_eq!(backend.pixel_size("ui", 0.001), Some(1.0));
}

#[test]
fn a_large_text_scale_on_a_large_font_still_draws() {
    let label = at(ttf(UiNode::label("big", "Big")), 0.0, 0.0, 300.0, 60.0)
        .with_style(|s| s.font_scale = 3.0);
    let fonts = json!({"schema":1,"defaultFamily":"ui",
        "families":{"ui":{"size":48,"atlas":"coverage"}}});
    let (_dir, mut runtime) = runtime_with(
        vec![ui_entity("hud", &root(vec![label]))],
        vec![json!({"id":"fonts","kind":"font_config","path":"fonts.json"})],
        &[("fonts.json", fonts)],
    );
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<AccessibilityControl>()
        .set_text_scale(4.0)
        .unwrap();
    tick(&mut runtime);
    runtime.extract_frame().expect("576 px text extracts");
}

// ---- unvalidated accessibility settings ------------------------------------

#[test]
fn ui_scaling_sanitises_settings_that_were_never_validated() {
    let mut world = World::new();
    world.insert_resource(AccessibilityControl {
        settings: AccessibilitySettings {
            ui_scale: 0.0,
            text_scale: f32::NAN,
            min_text_size: -5.0,
            ..AccessibilitySettings::default()
        },
        ..AccessibilityControl::default()
    });
    let scaling = UiScaling::of(&world);
    assert_eq!(scaling.ui_scale, 0.5);
    assert_eq!(scaling.text_scale, 1.0);
    assert_eq!(scaling.min_text_size, 0.0);
    assert_eq!(scaling.layout_viewport([800, 600]), [1600, 1200]);
    world
        .resource_mut::<AccessibilityControl>()
        .settings
        .ui_scale = 100.0;
    assert_eq!(UiScaling::of(&world).ui_scale, 4.0);
}

#[test]
fn a_zero_ui_scale_in_the_settings_resource_does_not_break_the_frame() {
    let tree = root(vec![at(
        ttf(UiNode::label("a", "Hi")),
        0.0,
        0.0,
        100.0,
        20.0,
    )]);
    let (_dir, mut runtime) = runtime_with(vec![ui_entity("hud", &tree)], vec![], &[]);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<AccessibilityControl>()
        .settings
        .ui_scale = 0.0;
    tick(&mut runtime);
    runtime.extract_frame().expect("extract");
}

// ---- world panels follow the screen-root settings ---------------------------

fn panel_entity(root: &UiRoot) -> Value {
    json!({"id":"panel","name":"panel","components":{
        "Transform":{"position":[0,0,0]},
        "custom":{
            "engine.UiRoot":{"version":1,"data":root},
            "engine.UiWorldPanel":{"version":1,"data":{}}}}})
}

fn panel_draws(runtime: &Runtime) -> Vec<engine_project::runtime::Draw> {
    runtime
        .extract_frame()
        .unwrap()
        .draws
        .into_iter()
        .filter(|d| d.id.starts_with("world:game-ui:"))
        .collect()
}

#[test]
fn world_panels_draw_the_focus_ring_with_the_accessibility_settings() {
    let mut toggle = UiNode::toggle("t", "", false).with_action("flip");
    toggle.style.width = UiLength::Fill;
    toggle.style.height = UiLength::Fill;
    toggle.style.padding = 0.0;
    let (_dir, mut runtime) = runtime_with(vec![panel_entity(&root(vec![toggle]))], vec![], &[]);
    // A click through the default camera focuses the toggle.
    runtime
        .tick(0.016, press("ui_click", [160.0, 90.0]))
        .unwrap();
    runtime
        .tick(
            0.016,
            ActionFrame {
                pointer: [160.0, 90.0],
                ..Default::default()
            },
        )
        .unwrap();
    let chrome_vertices = |runtime: &Runtime| -> usize {
        panel_draws(runtime)
            .iter()
            .filter(|d| d.id.ends_with(":0"))
            .map(|d| d.mesh.vertices.len())
            .sum()
    };
    let plain = chrome_vertices(&runtime);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<AccessibilityControl>()
        .settings
        .focus_high_contrast = true;
    let contrast = chrome_vertices(&runtime);
    assert!(
        contrast > plain,
        "high-contrast focus draws a second ring: {contrast} vs {plain}"
    );
}

#[test]
fn the_text_size_floor_of_a_world_panel_ignores_the_ui_scale() {
    let label = at(ttf(UiNode::label("l", "Mg")), 0.0, 0.0, 200.0, 100.0);
    let (_dir, mut runtime) = runtime_with(vec![panel_entity(&root(vec![label]))], vec![], &[]);
    tick(&mut runtime);
    let glyph_height = |runtime: &Runtime| -> f32 {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for d in panel_draws(runtime) {
            if d.id.rsplit(':').next().is_some_and(|s| s.starts_with('2')) {
                for v in &d.mesh.vertices {
                    lo = lo.min(v.position[1]);
                    hi = hi.max(v.position[1]);
                }
            }
        }
        hi - lo
    };
    let set = |runtime: &mut Runtime, ui_scale: f32| {
        let mut control = runtime
            .app_mut()
            .world_mut()
            .resource_mut::<AccessibilityControl>();
        let mut next = control.settings.clone();
        next.ui_scale = ui_scale;
        next.min_text_size = 60.0;
        control.set(next).unwrap();
    };
    set(&mut runtime, 1.0);
    let one = glyph_height(&runtime);
    set(&mut runtime, 2.0);
    let two = glyph_height(&runtime);
    assert!(one > 0.0, "the panel draws text");
    assert!(
        (one - two).abs() < 1e-4,
        "panels are not UI-scaled, so neither is their floor: {one} vs {two}"
    );
}
