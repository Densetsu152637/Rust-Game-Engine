use bevy_ecs::prelude::*;
use engine_project::{
    ActionFrame, EngineEvent, Project, Result, Runtime,
    content::{FontAtlas, FontGlyph, Texture},
    ui::Rect,
    ui_tree::*,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn node(id: &str, widget: UiWidget) -> UiNode {
    UiNode {
        id: id.into(),
        widget,
        style: UiStyle {
            padding: 0.0,
            ..Default::default()
        },
        ..Default::default()
    }
}
fn button(id: &str) -> UiNode {
    node(
        id,
        UiWidget::Button {
            text: String::new(),
            action: "play".into(),
        },
    )
}
fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}
fn frame(action: &str, pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pressed: [action.into()].into(),
        pointer,
        ..Default::default()
    }
}

#[test]
fn row_and_column_fill_use_inner_space_and_ignore_hidden_children() {
    let mut panel = node("panel", UiWidget::Container);
    panel.style = UiStyle {
        width: UiLength::Fill,
        height: UiLength::Fill,
        layout: UiLayout::Column,
        padding: 10.0,
        gap: 10.0,
        ..Default::default()
    };
    let mut row = node("row", UiWidget::Container);
    row.parent = Some("panel".into());
    row.style = UiStyle {
        width: UiLength::Fill,
        height: UiLength::Fill,
        layout: UiLayout::Row,
        padding: 0.0,
        gap: 10.0,
        ..Default::default()
    };
    let mut first = button("first");
    first.parent = Some("row".into());
    first.style.width = UiLength::Pixels(40.0);
    let mut second = button("second");
    second.parent = Some("row".into());
    second.style.width = UiLength::Fill;
    let mut hidden = button("hidden");
    hidden.parent = Some("row".into());
    hidden.visible = false;
    // Deliberately place children before their parent in storage.
    let tree = UiTree {
        nodes: vec![first, second, hidden, row, panel],
    };
    let boxes = tree.layout([200, 100]).unwrap();
    assert_eq!(
        boxes
            .iter()
            .map(|b| tree.nodes[b.node].id.as_str())
            .collect::<Vec<_>>(),
        ["panel", "row", "first", "second"]
    );
    assert_eq!(
        boxes[1].rect,
        Rect {
            x: 10.0,
            y: 10.0,
            width: 180.0,
            height: 80.0
        }
    );
    assert_eq!(
        boxes[3].rect,
        Rect {
            x: 60.0,
            y: 10.0,
            width: 130.0,
            height: 28.0
        }
    );
    assert_eq!(tree.layout([200, 100]).unwrap(), boxes);
}

#[test]
fn ancestor_clipping_matches_pointer_hit_area_and_can_be_disabled() {
    let mut panel = node("panel", UiWidget::Container);
    panel.style.width = UiLength::Pixels(50.0);
    let mut child = button("child");
    child.parent = Some("panel".into());
    child.style.offset = [40.0, 0.0];
    let mut roots = [("hud".into(), root(vec![panel, child]))];
    let mut ui = GameUi::default();
    assert!(
        ui.tick(&mut roots, [200, 100], &frame("ui_click", [55.0, 5.0]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ui.tick(&mut roots, [200, 100], &frame("ui_click", [45.0, 5.0]))
            .unwrap()[0]
            .node,
        "child"
    );
    roots[0].1.tree.nodes[0].style.clip = false;
    assert_eq!(
        ui.tick(&mut roots, [200, 100], &frame("ui_click", [55.0, 5.0]))
            .unwrap()[0]
            .node,
        "child"
    );
}

#[test]
fn invalid_graphs_numbers_and_depth_fail_before_interaction() {
    let mut tree = UiTree {
        nodes: vec![button("a"), button("b")],
    };
    tree.nodes[0].parent = Some("b".into());
    tree.nodes[1].parent = Some("a".into());
    assert!(tree.layout([320, 180]).is_err());
    tree.nodes[0].parent = None;
    tree.nodes[1].parent = Some("missing".into());
    assert!(tree.validate().is_err());
    tree.nodes[1].parent = None;
    tree.nodes[1].id = "a".into();
    assert!(tree.validate().is_err());
    tree.nodes[1].id = "b".into();
    tree.nodes[1].style.gap = f32::NAN;
    assert!(tree.validate().is_err());
    let nodes = (0..65)
        .map(|i| {
            let mut n = button(&i.to_string());
            n.parent = (i > 0).then(|| (i - 1).to_string());
            n
        })
        .collect();
    assert!(UiTree { nodes }.validate().is_err());
    assert!(UiTree::default().layout([0, 100]).is_err());
}

#[test]
fn keyboard_focus_skips_disabled_and_hidden_nodes_and_toggles_values() {
    let mut disabled = button("disabled");
    disabled.enabled = false;
    let mut hidden = button("hidden");
    hidden.visible = false;
    let toggle = node(
        "sound",
        UiWidget::Toggle {
            text: String::new(),
            checked: false,
            action: "sound".into(),
        },
    );
    let mut roots = [("hud".into(), root(vec![disabled, hidden, toggle]))];
    let mut ui = GameUi::default();
    ui.tick(&mut roots, [320, 180], &frame("ui_next", [0.0; 2]))
        .unwrap();
    assert_eq!(ui.focus().unwrap().node, "sound");
    let events = ui
        .tick(&mut roots, [320, 180], &frame("ui_confirm", [0.0; 2]))
        .unwrap();
    assert_eq!(events[0].name, "ui.value_changed");
    assert_eq!(events[0].value, true);
    assert!(matches!(
        roots[0].1.tree.nodes[2].widget,
        UiWidget::Toggle { checked: true, .. }
    ));
    roots[0].1.tree.nodes[2].visible = false;
    assert!(
        ui.tick(&mut roots, [320, 180], &frame("ui_confirm", [0.0; 2]))
            .unwrap()
            .is_empty()
    );
    assert!(ui.focus().is_none());
}

#[test]
fn pointer_uses_paint_order_and_keyboard_arrow_does_not_click_buttons() {
    let mut roots = [
        ("a".into(), root(vec![button("first"), button("last")])),
        ("a-".into(), root(vec![button("top")])),
    ];
    let mut ui = GameUi::default();
    let events = ui
        .tick(&mut roots, [320, 180], &frame("ui_click", [10.0, 10.0]))
        .unwrap();
    assert_eq!(events[0].root, "a-");
    assert_eq!(events[0].node, "top");
    assert!(
        ui.tick(&mut roots, [320, 180], &frame("ui_right", [0.0; 2]))
            .unwrap()
            .is_empty()
    );
    roots[0].1.layer = 10;
    assert_eq!(
        ui.tick(&mut roots, [320, 180], &frame("ui_click", [10.0, 10.0]))
            .unwrap()[0]
            .node,
        "last"
    );
}

#[test]
fn slider_captures_drag_quantizes_and_cancels_on_focus_loss() {
    let slider = node(
        "volume",
        UiWidget::Slider {
            value: 0.0,
            min: 0.0,
            max: 10.0,
            step: 2.0,
            action: "volume".into(),
        },
    );
    let mut roots = [("hud".into(), root(vec![slider]))];
    let mut ui = GameUi::default();
    assert_eq!(
        ui.tick(&mut roots, [320, 180], &frame("ui_click", [60.0, 10.0]))
            .unwrap()[0]
            .value,
        6.0
    );
    let mut drag = ActionFrame {
        held: ["ui_click".into()].into(),
        pointer: [500.0, 100.0],
        ..Default::default()
    };
    assert_eq!(
        ui.tick(&mut roots, [320, 180], &drag).unwrap()[0].value,
        10.0
    );
    drag.focused = false;
    ui.tick(&mut roots, [320, 180], &drag).unwrap();
    drag.focused = true;
    drag.pointer[0] = 0.0;
    assert!(ui.tick(&mut roots, [320, 180], &drag).unwrap().is_empty());
    assert_eq!(
        ui.tick(&mut roots, [320, 180], &frame("ui_left", [0.0; 2]))
            .unwrap()[0]
            .value,
        8.0
    );
}

#[test]
fn zero_sized_slider_does_not_divide_by_zero_or_change_values() {
    let mut slider = node(
        "volume",
        UiWidget::Slider {
            value: 0.0,
            min: 0.0,
            max: 1.0,
            step: 0.1,
            action: String::new(),
        },
    );
    slider.style.width = UiLength::Pixels(0.0);
    let mut roots = [("hud".into(), root(vec![slider]))];
    assert!(
        GameUi::default()
            .tick(&mut roots, [320, 180], &frame("ui_click", [0.0; 2]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn modal_panels_block_game_ui_and_cancel_slider_capture() {
    use engine_project::ui::{PanelOptions, UiModel};
    let slider = node(
        "volume",
        UiWidget::Slider {
            value: 0.0,
            min: 0.0,
            max: 1.0,
            step: 0.1,
            action: String::new(),
        },
    );
    let mut roots = [("hud".into(), root(vec![slider]))];
    let mut ui = GameUi::default();
    ui.tick(&mut roots, [320, 180], &frame("ui_click", [30.0, 10.0]))
        .unwrap();
    let mut modal = UiModel::default();
    modal
        .open(PanelOptions {
            id: "pause".into(),
            ..Default::default()
        })
        .unwrap();
    modal.tick(frame("ui_click", [100.0, 10.0]));
    assert!(
        ui.tick(&mut roots, [320, 180], &modal.gameplay_frame())
            .unwrap()
            .is_empty()
    );
    modal.close("pause");
    let drag = ActionFrame {
        held: ["ui_click".into()].into(),
        pointer: [100.0, 10.0],
        ..Default::default()
    };
    modal.tick(drag);
    assert!(
        ui.tick(&mut roots, [320, 180], &modal.gameplay_frame())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn slider_can_reach_endpoint_with_nondivisible_or_tiny_steps() {
    let slider = node(
        "volume",
        UiWidget::Slider {
            value: 0.0,
            min: 0.0,
            max: 10.0,
            step: 3.0,
            action: String::new(),
        },
    );
    let mut roots = [("hud".into(), root(vec![slider]))];
    let mut ui = GameUi::default();
    ui.tick(&mut roots, [320, 180], &frame("ui_click", [119.0, 10.0]))
        .unwrap();
    let drag = ActionFrame {
        held: ["ui_click".into()].into(),
        pointer: [200.0, 10.0],
        ..Default::default()
    };
    assert_eq!(
        ui.tick(&mut roots, [320, 180], &drag).unwrap()[0].value,
        10.0
    );
    roots[0].1.tree.nodes[0].widget = UiWidget::Slider {
        value: 0.0,
        min: 0.0,
        max: 1.0,
        step: f32::MIN_POSITIVE,
        action: String::new(),
    };
    let event = ui
        .tick(&mut roots, [320, 180], &frame("ui_click", [60.0, 10.0]))
        .unwrap();
    assert_eq!(event[0].value, 0.5);
}

fn font_assets() -> (BTreeMap<String, FontAtlas>, BTreeMap<String, Arc<Texture>>) {
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

#[test]
fn native_draws_contain_font_glyphs_progress_fill_focus_outline_and_clip() {
    let mut label = node("label", UiWidget::Label { text: "AA".into() });
    label.style.font = "font".into();
    label.style.width = UiLength::Pixels(8.0);
    let mut progress = node(
        "progress",
        UiWidget::Progress {
            value: 0.5,
            min: 0.0,
            max: 1.0,
        },
    );
    progress.style.offset = [0.0, 30.0];
    progress.style.width = UiLength::Pixels(100.0);
    let mut toggle = node(
        "toggle",
        UiWidget::Toggle {
            text: String::new(),
            checked: true,
            action: String::new(),
        },
    );
    toggle.style.offset = [0.0, 60.0];
    let root = root(vec![label, progress, toggle]);
    let (fonts, textures) = font_assets();
    let draws = root
        .extract_draws("hud", [320, 180], Some("toggle"), &fonts, &textures)
        .unwrap();
    assert_eq!(draws.len(), 4);
    let glyphs = draws.iter().find(|d| d.texture.is_some()).unwrap();
    assert_eq!(glyphs.mesh.vertices.len(), 8);
    assert_eq!(glyphs.clip, Some([0.0, 0.0, 8.0, 28.0]));
    assert_eq!(glyphs.mesh.vertices[7].position[1], 8.0); // second glyph wrapped
    assert_eq!(draws[2].mesh.vertices[5].position[0], 50.0); // half progress fill
    assert_eq!(draws[3].mesh.vertices.len(), 24); // background, check, four focus edges
    assert!(
        draws
            .iter()
            .all(|d| d.space == engine_project::runtime::DrawSpace::Ui && d.motion.is_none())
    );
}

#[test]
fn draw_ids_sort_in_same_root_and_preorder_order_as_hit_testing() {
    let first = root(vec![button("z"), button("a")]);
    let second = root(vec![button("x")]);
    let mut draws = first
        .extract_draws("a", [320, 180], None, &BTreeMap::new(), &BTreeMap::new())
        .unwrap();
    draws.extend(
        second
            .extract_draws("a-", [320, 180], None, &BTreeMap::new(), &BTreeMap::new())
            .unwrap(),
    );
    let original = draws.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
    draws.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        original,
        draws.iter().map(|d| d.id.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn missing_font_fails_explicitly_and_descriptor_accepts_typed_roundtrip() {
    let root = root(vec![node(
        "label",
        UiWidget::Label {
            text: "hello".into(),
        },
    )]);
    assert!(
        root.extract_draws("hud", [320, 180], None, &BTreeMap::new(), &BTreeMap::new())
            .is_err()
    );
    let catalog = engine_project::ComponentCatalog::new(vec![descriptor()]).unwrap();
    let value = engine_project::CustomValue {
        version: 1,
        data: serde_json::to_value(&root).unwrap(),
    };
    catalog.validate("engine.UiRoot", &value).unwrap();
    let decoded: UiRoot = serde_json::from_value(value.data).unwrap();
    assert_eq!(decoded, root);
}

#[derive(Resource, Default)]
struct Observed(Vec<serde_json::Value>);
fn callback(world: &mut World, owner: Entity, event: &EngineEvent) -> Result<()> {
    assert_eq!(event.source, Some(owner));
    assert_eq!(event.target, Some(owner));
    world
        .resource_mut::<Observed>()
        .0
        .push(event.payload.clone());
    Ok(())
}

#[test]
fn authored_ui_hydrates_changes_typed_state_and_delivers_native_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    let root = root(vec![node(
        "sound",
        UiWidget::Toggle {
            text: String::new(),
            checked: false,
            action: "mute".into(),
        },
    )]);
    std::fs::write(
        dir.path().join("components.json"),
        serde_json::to_vec(&vec![descriptor()]).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("scene.json"),serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":[{
        "id":"hud","name":"HUD","components":{"custom":{"engine.UiRoot":{"version":1,"data":root}},
        "EventBindings":{"bindings":[{"event":"ui.value_changed","handler":"sound"}]}}
    }]})).unwrap()).unwrap();
    std::fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"ui-test","startup_scene":"scene.json","componentSchemas":"components.json","assets":[]})).unwrap()).unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    register(runtime.app_mut()).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Observed::default());
    engine_project::register_event_handler(runtime.app_mut(), "sound", callback).unwrap();
    runtime.start().unwrap();
    tick_world(
        runtime.app_mut().world_mut(),
        [320, 180],
        &frame("ui_click", [5.0, 5.0]),
    )
    .unwrap();
    assert!(runtime.app().world().resource::<Observed>().0.is_empty());
    runtime.tick(1.0 / 60.0, ActionFrame::default()).unwrap();
    assert_eq!(
        runtime.app().world().resource::<Observed>().0,
        vec![json!({"node":"sound","action":"mute","value":true})]
    );
    let snapshot = runtime.try_snapshot().unwrap();
    assert_eq!(
        snapshot.scene.entities[0].components.custom["engine.UiRoot"].data["tree"]["nodes"][0]["widget"]
            ["checked"],
        true
    );
    assert!(
        !extract_world(
            runtime.app().world(),
            [320, 180],
            &BTreeMap::new(),
            &BTreeMap::new()
        )
        .unwrap()
        .is_empty()
    );
}
