//! Interaction contracts of the retained game UI: hit testing, navigation,
//! scrolling, popups, modal dialogs, tabs, tooltips, pointer-state flags and
//! the text-input event model.
use engine_project::{
    ActionFrame,
    content::{FontAtlas, FontGlyph, Texture},
    ui_tree::*,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

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
fn held(pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        held: ["ui_click".into()].into(),
        pointer,
        ..Default::default()
    }
}
fn hover(pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pointer,
        ..Default::default()
    }
}
fn focus(ui: &GameUi) -> &str {
    ui.focus().map_or("", |f| f.node.as_str())
}
fn widget<'a>(roots: &'a [(String, UiRoot)], id: &str) -> &'a UiWidget {
    let tree = &roots[0].1.tree;
    &tree.nodes[tree.find(id).unwrap()].widget
}

#[test]
fn hit_testing_respects_scroll_offset_clipping_and_clamps_offsets() {
    let mut view = at(
        UiNode::scroll_view("view", UiScrollAxes::Vertical),
        0.0,
        0.0,
        100.0,
        100.0,
    );
    view.style.layout = UiLayout::Column;
    if let UiWidget::ScrollView {
        offset, scrollbars, ..
    } = &mut view.widget
    {
        *offset = [0.0, 50.0];
        *scrollbars = false;
    }
    let mut nodes = vec![view];
    for i in 0..5 {
        nodes.push(
            UiNode::button(format!("b{i}"), "", "")
                .with_size(UiLength::Fill, px(40.0))
                .child_of("view"),
        );
    }
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = GameUi::default();
    let clicked = |ui: &mut GameUi, roots: &mut [(String, UiRoot)], y: f32| {
        ui.tick(roots, [320, 180], &press("ui_click", [10.0, y]))
            .unwrap()
            .first()
            .map(|e| e.node.clone())
    };
    assert_eq!(clicked(&mut ui, &mut roots, 5.0).as_deref(), Some("b1"));
    assert_eq!(clicked(&mut ui, &mut roots, 95.0).as_deref(), Some("b3"));
    // b3 extends to y=110 but the view clips at 100.
    assert_eq!(clicked(&mut ui, &mut roots, 105.0), None);
    if let UiWidget::ScrollView { offset, .. } = &mut roots[0].1.tree.nodes[0].widget {
        *offset = [0.0, 500.0];
    }
    let boxes = roots[0].1.tree.layout([320, 180]).unwrap();
    assert_eq!(
        (boxes[0].scroll, boxes[0].scroll_max),
        ([0.0, 100.0], [0.0, 100.0])
    );
    assert_eq!(clicked(&mut ui, &mut roots, 5.0).as_deref(), Some("b2"));
}

#[test]
fn wheel_and_drag_scroll_with_clamping_and_events() {
    let mut view = at(
        UiNode::scroll_view("view", UiScrollAxes::Vertical),
        0.0,
        0.0,
        100.0,
        100.0,
    );
    view.style.layout = UiLayout::Column;
    let mut nodes = vec![view];
    for i in 0..4 {
        nodes.push(
            UiNode::label(format!("l{i}"), "")
                .with_size(UiLength::Fill, px(50.0))
                .child_of("view"),
        );
    }
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = GameUi::default();
    let wheel = |y: f32| ActionFrame {
        pointer: [50.0, 50.0],
        wheel: [0.0, y],
        ..Default::default()
    };
    let events = ui.tick(&mut roots, [320, 180], &wheel(-1.0)).unwrap();
    assert_eq!(events[0].name, "ui.scrolled");
    assert_eq!(events[0].value, json!([0.0, 40.0]));
    let events = ui.tick(&mut roots, [320, 180], &wheel(-5.0)).unwrap();
    assert_eq!(events[0].value, json!([0.0, 100.0]), "clamped to content");
    assert!(
        ui.tick(&mut roots, [320, 180], &wheel(-1.0))
            .unwrap()
            .is_empty()
    );
    // Content drag: dragging down by 30 px scrolls back up by 30 px.
    ui.tick(&mut roots, [320, 180], &press("ui_click", [40.0, 20.0]))
        .unwrap();
    let events = ui
        .tick(&mut roots, [320, 180], &held([40.0, 50.0]))
        .unwrap();
    assert_eq!(events[0].value, json!([0.0, 70.0]));
    // Thumb drag: the 50 px thumb travels 50 px for 100 px of content.
    ui.tick(&mut roots, [320, 180], &ActionFrame::default())
        .unwrap();
    ui.tick(&mut roots, [320, 180], &press("ui_click", [96.0, 60.0]))
        .unwrap();
    let events = ui
        .tick(&mut roots, [320, 180], &held([96.0, 50.0]))
        .unwrap();
    assert_eq!(events[0].value, json!([0.0, 50.0]));
    assert!(
        matches!(widget(&roots, "view"), UiWidget::ScrollView { offset, .. } if *offset == [0.0, 50.0])
    );
}

fn grid() -> Vec<UiNode> {
    let mut nodes = vec![];
    for row in 0..3 {
        for col in 0..3 {
            nodes.push(at(
                UiNode::button(format!("b{row}{col}"), "", ""),
                col as f32 * 100.0,
                row as f32 * 100.0,
                40.0,
                40.0,
            ));
        }
    }
    nodes
}

#[test]
fn spatial_navigation_picks_nearest_in_each_direction_and_honours_overrides() {
    let mut roots = [("hud".to_string(), root(grid()))];
    let mut ui = GameUi::default();
    let go = |ui: &mut GameUi, roots: &mut [(String, UiRoot)], action: &str| {
        ui.tick(roots, [400, 400], &press(action, [-1.0, -1.0]))
            .unwrap();
        focus(ui).to_string()
    };
    ui.tick(&mut roots, [400, 400], &press("ui_click", [110.0, 110.0]))
        .unwrap();
    assert_eq!(focus(&ui), "b11");
    assert_eq!(go(&mut ui, &mut roots, "ui_right"), "b12");
    assert_eq!(
        go(&mut ui, &mut roots, "ui_right"),
        "b12",
        "nothing further right"
    );
    assert_eq!(go(&mut ui, &mut roots, "ui_down"), "b22");
    assert_eq!(go(&mut ui, &mut roots, "ui_left"), "b21");
    assert_eq!(go(&mut ui, &mut roots, "ui_up"), "b11");
    assert_eq!(go(&mut ui, &mut roots, "ui_up"), "b01");
    // Of two neighbours to the right, the one with less perpendicular gap wins.
    roots[0].1.tree.nodes[2].style.offset = [200.0, 60.0];
    assert_eq!(go(&mut ui, &mut roots, "ui_right"), "b02");
    // Explicit overrides win over spatial search.
    roots[0].1.tree.nodes[2].nav.down = Some("b20".into());
    assert_eq!(go(&mut ui, &mut roots, "ui_down"), "b20");
    // Tab order: ascending tabIndex, then paint order; wraps.
    roots[0].1.tree.nodes[8].nav.tab_index = -1;
    roots[0].1.tree.nodes[4].nav.focusable = Some(false);
    ui.clear();
    assert_eq!(go(&mut ui, &mut roots, "ui_next"), "b22");
    assert_eq!(go(&mut ui, &mut roots, "ui_next"), "b00");
    assert_eq!(go(&mut ui, &mut roots, "ui_previous"), "b22");
    let order: Vec<_> = (0..8).map(|_| go(&mut ui, &mut roots, "ui_next")).collect();
    assert!(
        !order.contains(&"b11".to_string()),
        "focusable=false is skipped: {order:?}"
    );
}

#[test]
fn pointer_state_flags_track_hover_press_focus_disable_and_check() {
    let mut toggle = at(UiNode::toggle("t", "", true), 0.0, 50.0, 40.0, 40.0);
    toggle.enabled = true;
    let mut off = at(UiNode::button("off", "", ""), 100.0, 0.0, 40.0, 40.0);
    off.enabled = false;
    let mut roots = [(
        "hud".to_string(),
        root(vec![
            at(UiNode::button("b", "", ""), 0.0, 0.0, 40.0, 40.0),
            toggle,
            off,
        ]),
    )];
    let mut ui = GameUi::default();
    ui.tick(&mut roots, [400, 400], &hover([10.0, 10.0]))
        .unwrap();
    assert!(ui.state("hud", "b").hovered());
    assert!(!ui.state("hud", "b").pressed());
    assert!(ui.state("hud", "t").checked());
    assert!(ui.state("hud", "off").disabled());
    let events = ui
        .tick(&mut roots, [400, 400], &press("ui_click", [10.0, 10.0]))
        .unwrap();
    assert_eq!(events[0].name, "ui.clicked");
    let flags = ui.state("hud", "b");
    assert!(flags.hovered() && flags.pressed() && flags.focused());
    assert_eq!(ui.render_state("hud").pressed.as_deref(), Some("b"));
    ui.tick(&mut roots, [400, 400], &held([10.0, 10.0]))
        .unwrap();
    assert!(ui.state("hud", "b").pressed());
    ui.tick(&mut roots, [400, 400], &held([300.0, 300.0]))
        .unwrap();
    assert!(
        !ui.state("hud", "b").pressed(),
        "pressed needs the pointer over the widget"
    );
    ui.tick(&mut roots, [400, 400], &hover([300.0, 300.0]))
        .unwrap();
    assert_eq!(ui.state("hud", "b"), UiStateFlags::FOCUSED);
}

#[test]
fn dropdown_popup_draws_above_other_roots_and_selects() {
    let dropdown = at(
        UiNode::dropdown("lang", vec!["en".into(), "fr".into(), "de".into()], None)
            .with_action("lang"),
        0.0,
        0.0,
        100.0,
        20.0,
    );
    let mut cover = root(vec![at(
        UiNode::button("cover", "", "covered"),
        0.0,
        20.0,
        100.0,
        60.0,
    )]);
    cover.layer = 5;
    let mut roots = [
        ("a".to_string(), root(vec![dropdown])),
        ("b".to_string(), cover),
    ];
    let mut ui = GameUi::default();
    assert!(
        ui.tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(ui.open_popup().unwrap().node, "lang");
    assert!(ui.state("a", "lang").open());
    // Option 1 lies over the higher-layer button; the popup wins.
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [10.0, 45.0]))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].name.as_str(), &events[0].value),
        ("ui.selection_changed", &json!(1))
    );
    assert_eq!(events[0].action, "lang");
    assert!(ui.open_popup().is_none());
    assert!(matches!(
        widget(&roots, "lang"),
        UiWidget::Dropdown {
            selected: Some(1),
            ..
        }
    ));
    // With the popup closed the same point reaches the button.
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [10.0, 45.0]))
        .unwrap();
    assert_eq!(events[0].node, "cover");
    // Keyboard: confirm opens, down moves the highlight, confirm selects.
    ui.tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
        .unwrap();
    ui.tick(&mut roots, [400, 300], &press("ui_cancel", [-1.0, -1.0]))
        .unwrap();
    assert!(ui.open_popup().is_none());
    ui.tick(&mut roots, [400, 300], &press("ui_confirm", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(ui.render_state("a").popup, Some(("lang".into(), 1, 1)));
    ui.tick(&mut roots, [400, 300], &press("ui_down", [-1.0, -1.0]))
        .unwrap();
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_confirm", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(events[0].value, json!(2));
    // Clicking elsewhere closes the popup without selecting.
    ui.tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
        .unwrap();
    assert!(
        ui.tick(&mut roots, [400, 300], &press("ui_click", [300.0, 200.0]))
            .unwrap()
            .is_empty()
    );
    assert!(ui.open_popup().is_none());
    // Drawn above every root.
    ui.tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
        .unwrap();
    let draws = roots[0]
        .1
        .extract_draws_with(
            "a",
            [400, 300],
            &ui.render_state("a"),
            &NullTextBackend,
            &BTreeMap::new(),
        )
        .unwrap();
    assert!(draws.iter().any(|d| d.layer == UI_POPUP_LAYER));
}

#[test]
fn modal_dialog_blocks_input_beneath_traps_focus_and_reports_result() {
    let mut dialog = UiNode::dialog("quit", "Quit?", vec!["Yes".into(), "No".into()])
        .with_size(px(200.0), px(100.0))
        .with_action("quit");
    dialog.style.padding = 0.0;
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let inner = UiNode::button("inner", "", "")
        .with_size(px(50.0), px(20.0))
        .child_of("quit");
    let behind = at(UiNode::button("behind", "", "play"), 0.0, 0.0, 100.0, 40.0);
    let behind2 = at(
        UiNode::button("behind2", "", "play"),
        0.0,
        260.0,
        100.0,
        40.0,
    );
    let mut other = root(vec![at(
        UiNode::button("hud", "", ""),
        300.0,
        0.0,
        100.0,
        40.0,
    )]);
    other.layer = 9;
    let mut roots = [
        (
            "menu".to_string(),
            root(vec![behind, dialog, inner, behind2]),
        ),
        ("z".to_string(), other),
    ];
    let mut ui = GameUi::default();
    // Clicks beneath the modal (any root, any layer) are swallowed.
    assert!(
        ui.tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
            .unwrap()
            .is_empty()
    );
    assert!(
        ui.tick(&mut roots, [400, 300], &press("ui_click", [310.0, 10.0]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(focus(&ui), "quit");
    assert!(ui.state("menu", "quit").open());
    // Tab order cycles inside the dialog only.
    let mut seen = vec![];
    for _ in 0..4 {
        ui.tick(&mut roots, [400, 300], &press("ui_next", [-1.0, -1.0]))
            .unwrap();
        seen.push(focus(&ui).to_string());
    }
    assert_eq!(seen, ["inner", "quit", "inner", "quit"]);
    // The inner button works: it is in the modal subtree. Centred at (100, 100).
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [110.0, 140.0]))
        .unwrap();
    assert_eq!(events[0].node, "inner");
    // Footer button "No" (index 1): row 172..200, cell x 204..296.
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [250.0, 185.0]))
        .unwrap();
    assert_eq!(
        (events[0].name.as_str(), &events[0].value),
        ("ui.dialog_result", &json!(1))
    );
    assert_eq!(events[0].action, "quit");
    assert!(matches!(
        widget(&roots, "quit"),
        UiWidget::ModalDialog { open: false, .. }
    ));
    // Closed: the underlying buttons receive clicks again.
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [10.0, 10.0]))
        .unwrap();
    assert_eq!(events[0].node, "behind");
    // Keyboard: right highlights "No"? cancel returns -1.
    if let UiWidget::ModalDialog { open, .. } = &mut roots[0].1.tree.nodes[1].widget {
        *open = true;
    }
    ui.tick(&mut roots, [400, 300], &ActionFrame::default())
        .unwrap();
    ui.tick(&mut roots, [400, 300], &press("ui_right", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(
        ui.render_state("menu").dialog_choice,
        Some(("quit".into(), 1))
    );
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_cancel", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(events[0].value, json!(-1));
}

#[test]
fn tabs_switch_the_visible_panel() {
    let tabs = at(
        UiNode::tabs("tabs", vec!["A".into(), "B".into()]),
        0.0,
        0.0,
        300.0,
        200.0,
    )
    .with_action("tab");
    let panel = |id: &str| {
        UiNode::new(id, UiWidget::Container)
            .with_size(UiLength::Fill, UiLength::Fill)
            .child_of("tabs")
    };
    let mut roots = [(
        "hud".to_string(),
        root(vec![tabs, panel("pa"), panel("pb")]),
    )];
    let visible = |roots: &[(String, UiRoot)]| {
        let tree = &roots[0].1.tree;
        tree.layout([400, 300])
            .unwrap()
            .iter()
            .map(|b| tree.nodes[b.node].id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(visible(&roots), ["tabs", "pa"]);
    let boxes = roots[0].1.tree.layout([400, 300]).unwrap();
    assert_eq!(boxes[1].rect.y, 28.0, "panel starts below the header");
    let mut ui = GameUi::default();
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [225.0, 10.0]))
        .unwrap();
    assert_eq!(
        (events[0].name.as_str(), &events[0].value),
        ("ui.tab_changed", &json!(1))
    );
    assert_eq!(visible(&roots), ["tabs", "pb"]);
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_left", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(events[0].value, json!(0));
    assert_eq!(visible(&roots), ["tabs", "pa"]);
}

#[test]
fn lists_and_virtual_lists_select_by_pointer_and_keyboard() {
    let list = at(UiNode::list("list"), 0.0, 0.0, 100.0, 100.0).with_action("pick");
    let item = |id: &str| {
        UiNode::label(id, "")
            .with_size(UiLength::Fill, px(20.0))
            .child_of("list")
    };
    let mut virtual_list = at(
        UiNode::virtual_list("rows", 100, 20.0, "row"),
        200.0,
        0.0,
        100.0,
        100.0,
    );
    if let UiWidget::VirtualList { scroll, .. } = &mut virtual_list.widget {
        *scroll = 30.0;
    }
    let row = UiNode::label("row", "")
        .with_size(UiLength::Fill, UiLength::Fill)
        .child_of("rows");
    let mut roots = [(
        "hud".to_string(),
        root(vec![
            list,
            item("a"),
            item("b"),
            item("c"),
            virtual_list,
            row,
        ]),
    )];
    let mut ui = GameUi::default();
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [10.0, 25.0]))
        .unwrap();
    assert_eq!(
        (events[0].node.as_str(), &events[0].value),
        ("list", &json!(1))
    );
    assert!(ui.state("hud", "b").contains(UiStateFlags::SELECTED));
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_down", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(events[0].value, json!(2));
    // At the last item, down leaves the list instead.
    ui.tick(&mut roots, [400, 300], &press("ui_down", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(focus(&ui), "list", "no focusable below");
    // Virtual list: y=15 + scroll 30 -> item 2.
    let events = ui
        .tick(&mut roots, [400, 300], &press("ui_click", [210.0, 15.0]))
        .unwrap();
    assert_eq!(
        (events[0].node.as_str(), &events[0].value),
        ("rows", &json!(2))
    );
    for _ in 0..4 {
        ui.tick(&mut roots, [400, 300], &press("ui_down", [-1.0, -1.0]))
            .unwrap();
    }
    // Item 6 (120..140) is revealed: scroll = 140 - 100.
    assert!(matches!(
        widget(&roots, "rows"),
        UiWidget::VirtualList { selected: Some(6), scroll, .. } if *scroll == 40.0
    ));
}

#[test]
fn tooltips_appear_after_their_delay_and_hide_when_leaving() {
    let button = at(UiNode::button("b", "", ""), 0.0, 0.0, 50.0, 50.0).with_tooltip("Play", 0.5);
    let mut roots = [("hud".to_string(), root(vec![button]))];
    let mut ui = GameUi::default();
    let step = |ui: &mut GameUi, roots: &mut [(String, UiRoot)], pointer: [f32; 2]| {
        ui.tick_with(
            roots,
            &hover(pointer),
            &UiTickContext {
                viewport: [400, 300],
                dt: 0.2,
                text: &NullTextBackend,
            },
        )
        .unwrap();
        ui.tooltip()
            .map(|(k, t, at)| (k.node.clone(), t.to_string(), at))
    };
    assert_eq!(step(&mut ui, &mut roots, [10.0, 10.0]), None);
    assert_eq!(step(&mut ui, &mut roots, [12.0, 10.0]), None);
    assert_eq!(step(&mut ui, &mut roots, [14.0, 10.0]), None);
    assert_eq!(
        step(&mut ui, &mut roots, [16.0, 10.0]),
        Some(("b".into(), "Play".into(), [16.0, 10.0]))
    );
    assert!(ui.render_state("hud").tooltip.is_some());
    assert_eq!(step(&mut ui, &mut roots, [200.0, 200.0]), None);
}

fn assets() -> (BTreeMap<String, FontAtlas>, BTreeMap<String, Arc<Texture>>) {
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

fn input(max_length: u32, password: bool, multiline: bool) -> UiNode {
    let mut node = at(
        UiNode::text_input("name", "", max_length),
        0.0,
        0.0,
        200.0,
        40.0,
    )
    .with_action("name");
    node.style.font = "font".into();
    if let UiWidget::TextInput {
        password: p,
        multiline: m,
        ..
    } = &mut node.widget
    {
        *p = password;
        *m = multiline;
    }
    node
}
fn text_state(roots: &[(String, UiRoot)]) -> (String, u32, u32) {
    match widget(roots, "name") {
        UiWidget::TextInput {
            text,
            caret,
            anchor,
            ..
        } => (text.clone(), *caret, *anchor),
        _ => unreachable!(),
    }
}
fn key(key: UiEditKey) -> UiTextEvent {
    UiTextEvent::key(key)
}

#[test]
fn text_input_edits_select_clip_and_limit_length() {
    let (fonts, textures) = assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &backend,
    };
    let mut roots = [("hud".to_string(), root(vec![input(10, false, false)]))];
    let mut ui = GameUi::default();
    let run = |ui: &mut GameUi, roots: &mut [(String, UiRoot)], events: Vec<UiTextEvent>| {
        for e in events {
            ui.push_text_event(e).unwrap();
        }
        ui.tick_with(roots, &ActionFrame::default(), &ctx).unwrap()
    };
    // Events without a focused input are dropped.
    assert!(run(&mut ui, &mut roots, vec![UiTextEvent::Insert("x".into())]).is_empty());
    ui.tick_with(&mut roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    assert_eq!(ui.text_input_focus().unwrap().node, "name");
    let events = run(
        &mut ui,
        &mut roots,
        vec![
            UiTextEvent::Insert("hello".into()),
            key(UiEditKey::Left),
            key(UiEditKey::Left),
            UiTextEvent::Insert("X\u{7}".into()),
        ],
    );
    assert_eq!(events.len(), 1, "one change event per tick");
    assert_eq!(
        (events[0].name.as_str(), &events[0].value),
        ("ui.text_changed", &json!("helXlo"))
    );
    assert_eq!(text_state(&roots), ("helXlo".into(), 4, 4));
    // Select all, copy, collapse to the end, paste: truncated to max length 10.
    run(
        &mut ui,
        &mut roots,
        vec![
            key(UiEditKey::SelectAll),
            key(UiEditKey::Copy),
            key(UiEditKey::Right),
            key(UiEditKey::Paste),
        ],
    );
    assert_eq!(text_state(&roots), ("helXlohelX".into(), 10, 10));
    // Shift selection + typing replaces; word backspace deletes a word.
    let shift = UiEditModifiers {
        shift: true,
        word: false,
    };
    run(
        &mut ui,
        &mut roots,
        vec![
            key(UiEditKey::Home),
            UiTextEvent::Key {
                key: UiEditKey::Right,
                modifiers: shift,
            },
            UiTextEvent::Key {
                key: UiEditKey::Right,
                modifiers: shift,
            },
            UiTextEvent::Insert("Y".into()),
        ],
    );
    assert_eq!(text_state(&roots), ("YlXlohelX".into(), 1, 1));
    // Length limit truncates typing; backspace/delete remove single characters.
    run(
        &mut ui,
        &mut roots,
        vec![key(UiEditKey::End), UiTextEvent::Insert("ab".into())],
    );
    assert_eq!(text_state(&roots).0, "YlXlohelXa");
    run(
        &mut ui,
        &mut roots,
        vec![
            key(UiEditKey::Backspace),
            key(UiEditKey::Home),
            key(UiEditKey::Delete),
        ],
    );
    assert_eq!(text_state(&roots).0, "lXlohelX");
    // Word backspace removes the word before the caret.
    let word = UiEditModifiers {
        shift: false,
        word: true,
    };
    run(
        &mut ui,
        &mut roots,
        vec![
            key(UiEditKey::End),
            UiTextEvent::Insert(" q".into()),
            UiTextEvent::Key {
                key: UiEditKey::Backspace,
                modifiers: word,
            },
            key(UiEditKey::Backspace),
        ],
    );
    assert_eq!(text_state(&roots).0, "lXlohelX");
    // Cut removes the selection into the clipboard.
    run(
        &mut ui,
        &mut roots,
        vec![
            key(UiEditKey::End),
            UiTextEvent::Key {
                key: UiEditKey::Left,
                modifiers: shift,
            },
            key(UiEditKey::Cut),
            key(UiEditKey::Home),
            key(UiEditKey::Paste),
        ],
    );
    assert_eq!(text_state(&roots).0, "XlXlohel");
    let events = run(&mut ui, &mut roots, vec![key(UiEditKey::Enter)]);
    assert_eq!(
        (events[0].name.as_str(), &events[0].value),
        ("ui.text_submitted", &json!("XlXlohel"))
    );
    // Pointer placement: 5 px glyphs, x=12 is nearest the stop at 10 -> index 2.
    ui.tick_with(&mut roots, &press("ui_click", [12.0, 5.0]), &ctx)
        .unwrap();
    assert_eq!(text_state(&roots).1, 2);
    // Drag extends the selection, anchor stays.
    ui.tick_with(&mut roots, &held([31.0, 5.0]), &ctx).unwrap();
    assert_eq!(text_state(&roots), ("XlXlohel".into(), 6, 2));
    let area = ui.ime_area().unwrap();
    assert_eq!((area.x, area.y, area.height), (30.0, 0.0, 8.0));
    // Escape leaves the input.
    run(&mut ui, &mut roots, vec![key(UiEditKey::Escape)]);
    assert!(ui.focus().is_none() && ui.text_input_focus().is_none());
    // Hostile input is bounded.
    assert!(
        ui.push_text_event(UiTextEvent::Insert("x".repeat(2000)))
            .is_err()
    );
    for _ in 0..MAX_TEXT_EVENTS {
        ui.push_text_event(key(UiEditKey::Left)).unwrap();
    }
    let overflow = ui.push_text_event(key(UiEditKey::Left)).unwrap_err();
    assert_eq!(overflow.diagnostics[0].code, "ui.text_event.overflow");
}

#[test]
fn ime_preedit_and_commit_and_password_clipboard_rules() {
    let (fonts, textures) = assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let ctx = UiTickContext {
        viewport: [400, 300],
        dt: 0.0,
        text: &backend,
    };
    let mut roots = [("hud".to_string(), root(vec![input(32, true, false)]))];
    let mut ui = GameUi::default();
    ui.tick_with(&mut roots, &press("ui_click", [5.0, 5.0]), &ctx)
        .unwrap();
    ui.push_text_event(UiTextEvent::Insert("pw".into()))
        .unwrap();
    ui.push_text_event(UiTextEvent::Preedit(UiPreedit {
        text: "かな".into(),
        cursor: Some([0, 3]),
    }))
    .unwrap();
    let events = ui
        .tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    assert_eq!(
        events[0].value,
        json!("pw"),
        "preedit is not part of the value"
    );
    assert_eq!(ui.preedit().unwrap().text, "かな");
    assert!(ui.render_state("hud").preedit.is_some());
    // IME caret sits at the preedit cursor: after "**" (10 px) + one glyph (5 px).
    assert_eq!(ui.ime_area().unwrap().x, 15.0);
    ui.push_text_event(UiTextEvent::Commit("仮名".into()))
        .unwrap();
    let events = ui
        .tick_with(&mut roots, &ActionFrame::default(), &ctx)
        .unwrap();
    assert_eq!(events[0].value, json!("pw仮名"));
    assert!(ui.preedit().is_none());
    assert_eq!(text_state(&roots).1, 4, "caret counts characters");
    // Password values never reach the clipboard.
    let clipboard = Arc::new(MemoryClipboard::default());
    clipboard.set("kept");
    ui.set_clipboard(clipboard.clone());
    for e in [
        key(UiEditKey::SelectAll),
        key(UiEditKey::Copy),
        key(UiEditKey::Cut),
    ] {
        ui.push_text_event(e).unwrap();
    }
    assert!(
        ui.tick_with(&mut roots, &ActionFrame::default(), &ctx)
            .unwrap()
            .is_empty()
    );
    assert_eq!(clipboard.get().as_deref(), Some("kept"));
    assert_eq!(text_state(&roots).0, "pw仮名");
    // Invalid preedit cursor ranges are rejected at the boundary.
    let bad = UiTextEvent::Preedit(UiPreedit {
        text: "かな".into(),
        cursor: Some([0, 1]),
    });
    assert_eq!(
        ui.push_text_event(bad).unwrap_err().diagnostics[0].code,
        "ui.text_event.invalid"
    );
}

#[test]
fn multiline_input_moves_by_lines_and_keeps_newlines() {
    let mut roots = [("hud".to_string(), root(vec![input(64, false, true)]))];
    let mut ui = GameUi::default();
    ui.tick(&mut roots, [400, 300], &press("ui_click", [5.0, 5.0]))
        .unwrap();
    for e in [
        UiTextEvent::Insert("abc".into()),
        key(UiEditKey::Enter),
        UiTextEvent::Insert("de".into()),
        key(UiEditKey::Up),
    ] {
        ui.push_text_event(e).unwrap();
    }
    let events = ui
        .tick(&mut roots, [400, 300], &ActionFrame::default())
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "enter inserts a newline instead of submitting"
    );
    assert_eq!(text_state(&roots), ("abc\nde".into(), 2, 2));
    for e in [key(UiEditKey::End), key(UiEditKey::Down)] {
        ui.push_text_event(e).unwrap();
    }
    ui.tick(&mut roots, [400, 300], &ActionFrame::default())
        .unwrap();
    assert_eq!(text_state(&roots).1, 6, "column clamps to the shorter line");
    // Directional actions do not move focus away from an editing input.
    ui.tick(&mut roots, [400, 300], &press("ui_down", [-1.0, -1.0]))
        .unwrap();
    assert_eq!(focus(&ui), "name");
}
