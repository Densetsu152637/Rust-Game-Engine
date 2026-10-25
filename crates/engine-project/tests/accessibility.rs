#![recursion_limit = "512"]
//! Accessibility: the neutral tree (roles, names, states, modal and tab
//! hiding, incremental updates), assistive-technology action requests,
//! settings (UI scale, text scale, reduced motion, high-contrast theme), the
//! authored `accessibility` field and descriptors, and the remapping screen.
use bevy_ecs::prelude::{Entity, Resource, World};
use engine_project::{
    ActionFrame, AuthoredId, ComponentCatalog, CustomValue, EngineEvent, Project, Result, Runtime,
    accessibility::{AccessBridgeStatus, AccessibilityControl, AccessibilitySettings, UiScaling},
    content::{FontAtlas, FontGlyph, Texture},
    input::BindingMap,
    remap_ui::{
        RawInputTokens, RemapConflicts, RemapController, RemapEvent, RemapOptions, RemapScreen,
    },
    runtime::InputSettings,
    ui_tree::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

const VIEWPORT: [u32; 2] = [640, 360];

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

fn idle() -> ActionFrame {
    ActionFrame::default()
}

fn press(action: &str, pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pressed: [action.into()].into(),
        pointer,
        ..Default::default()
    }
}

fn ui_with_tree() -> GameUi {
    let mut ui = GameUi::default();
    ui.set_accessibility(true);
    ui
}

fn tick(
    ui: &mut GameUi,
    roots: &mut [(String, UiRoot)],
    input: &ActionFrame,
) -> Vec<UiWidgetEvent> {
    ui.tick(roots, VIEWPORT, input).unwrap()
}

fn request(ui: &mut GameUi, root: &str, node: &str, action: UiAccessAction) {
    ui.request_access(UiAccessRequest {
        target: AccessId::node(root, node),
        action,
    })
    .unwrap();
}

fn tree(ui: &GameUi) -> &AccessTree {
    ui.accessibility_tree().expect("accessibility tree")
}

fn node<'a>(ui: &'a GameUi, root: &str, id: &str) -> &'a AccessNode {
    tree(ui)
        .find(root, id)
        .unwrap_or_else(|| panic!("no accessibility node {id}"))
}

fn child_of(mut n: UiNode, parent: &str) -> UiNode {
    n.parent = Some(parent.into());
    n
}

// ---- roles, names and states ----------------------------------------------

fn every_widget_root() -> UiRoot {
    let list = at(UiNode::list("list"), 0.0, 300.0, 100.0, 60.0);
    let mut nodes = vec![
        at(
            UiNode::container("box", UiLayout::Column),
            0.0,
            0.0,
            10.0,
            10.0,
        ),
        at(UiNode::label("label", "Score"), 20.0, 0.0, 60.0, 10.0),
        at(
            UiNode::rich_label("rich", "<b>Bold</b> text"),
            90.0,
            0.0,
            60.0,
            10.0,
        ),
        at(
            UiNode::image("image", "tex", UiImageFit::Stretch),
            160.0,
            0.0,
            10.0,
            10.0,
        ),
        at(
            UiNode::nine_slice("frame", "tex", [1.0; 4]),
            180.0,
            0.0,
            10.0,
            10.0,
        ),
        at(
            UiNode::button("play", "Play", "play"),
            0.0,
            20.0,
            80.0,
            20.0,
        ),
        at(
            UiNode::toggle("sound", "Sound", true),
            0.0,
            50.0,
            80.0,
            20.0,
        ),
        at(
            UiNode::slider("volume", 0.5, 0.0, 1.0, 0.25),
            0.0,
            80.0,
            100.0,
            20.0,
        )
        .with_access_label("Volume"),
        at(
            UiNode::progress("loading", 30.0, 0.0, 100.0),
            0.0,
            110.0,
            100.0,
            10.0,
        )
        .with_access_label("Loading"),
        at(
            UiNode::text_input("name", "Your name", 20),
            0.0,
            130.0,
            100.0,
            20.0,
        ),
        at(
            UiNode::text_input("secret", "Password", 20),
            0.0,
            160.0,
            100.0,
            20.0,
        ),
        at(
            UiNode::dropdown("quality", vec!["Low".into(), "High".into()], Some(1)),
            0.0,
            190.0,
            100.0,
            20.0,
        )
        .with_access_label("Quality"),
        at(
            UiNode::scroll_view("scroll", UiScrollAxes::Vertical),
            120.0,
            20.0,
            100.0,
            100.0,
        ),
        list,
        child_of(UiNode::label("item0", "First"), "list"),
        child_of(UiNode::label("item1", "Second"), "list"),
        at(
            UiNode::dialog("confirm", "Quit?", vec!["Yes".into(), "No".into()]),
            0.0,
            0.0,
            200.0,
            100.0,
        ),
    ];
    let name = nodes.iter_mut().find(|n| n.id == "name").unwrap();
    if let UiWidget::TextInput { text, .. } = &mut name.widget {
        *text = "Ada".into();
    }
    let secret = nodes.iter_mut().find(|n| n.id == "secret").unwrap();
    if let UiWidget::TextInput { text, password, .. } = &mut secret.widget {
        *text = "hunter2".into();
        *password = true;
    }
    if let UiWidget::List { selected, .. } =
        &mut nodes.iter_mut().find(|n| n.id == "list").unwrap().widget
    {
        *selected = Some(1);
    }
    root(nodes)
}

#[test]
fn roles_names_values_and_states_map_from_every_widget_kind() {
    let mut roots = [("hud".to_string(), every_widget_root())];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let n = |id: &str| node(&ui, "hud", id).clone();

    assert_eq!(n("box").role, AccessRole::Group);
    let label = n("label");
    assert_eq!(
        (label.role, label.name.as_deref()),
        (AccessRole::Label, Some("Score"))
    );
    let rich = n("rich");
    assert_eq!(
        (rich.role, rich.name.as_deref()),
        (AccessRole::Label, Some("Bold text"))
    );
    assert_eq!(n("image").role, AccessRole::Image);
    assert_eq!(n("frame").role, AccessRole::Image);

    let play = n("play");
    assert_eq!(
        (play.role, play.name.as_deref()),
        (AccessRole::Button, Some("Play"))
    );
    assert!(play.states.focusable && !play.states.disabled);
    assert!(
        play.actions.contains(&AccessAction::Click) && play.actions.contains(&AccessAction::Focus)
    );

    let sound = n("sound");
    assert_eq!(
        (sound.role, sound.states.checked),
        (AccessRole::CheckBox, Some(true))
    );
    assert_eq!(sound.name.as_deref(), Some("Sound"));

    let volume = n("volume");
    assert_eq!(
        (volume.role, volume.name.as_deref()),
        (AccessRole::Slider, Some("Volume"))
    );
    assert_eq!(
        volume.value,
        Some(AccessValue::Number {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.25
        })
    );
    for action in [
        AccessAction::Increment,
        AccessAction::Decrement,
        AccessAction::SetValue,
    ] {
        assert!(volume.actions.contains(&action), "{action:?}");
    }

    let loading = n("loading");
    assert_eq!(loading.role, AccessRole::ProgressBar);
    assert_eq!(
        loading.value,
        Some(AccessValue::Number {
            value: 30.0,
            min: 0.0,
            max: 100.0,
            step: 0.0
        })
    );
    assert!(
        loading
            .actions
            .iter()
            .all(|a| *a == AccessAction::ScrollIntoView)
    );

    let name = n("name");
    assert_eq!(name.role, AccessRole::TextInput);
    assert_eq!(name.value, Some(AccessValue::Text("Ada".into())));
    assert_eq!(name.placeholder.as_deref(), Some("Your name"));
    assert!(name.actions.contains(&AccessAction::SetValue));
    let secret = n("secret");
    assert_eq!(secret.role, AccessRole::PasswordInput);
    assert!(secret.states.password);
    assert_eq!(secret.value, None, "a password is never exposed");

    let quality = n("quality");
    assert_eq!(quality.role, AccessRole::ComboBox);
    assert_eq!(quality.value, Some(AccessValue::Text("High".into())));
    assert_eq!(quality.states.expanded, Some(false));
    assert!(quality.actions.contains(&AccessAction::Expand));

    let scroll = n("scroll");
    assert_eq!(scroll.role, AccessRole::ScrollView);
    assert!(scroll.clips_children && scroll.scroll.is_some());

    let list = n("list");
    assert_eq!(list.role, AccessRole::List);
    assert_eq!(list.children.len(), 2);
    let first = n("item0");
    let second = n("item1");
    assert_eq!(first.role, AccessRole::ListItem);
    assert_eq!(
        (first.states.selected, second.states.selected),
        (Some(false), Some(true))
    );
    assert_eq!(second.position, Some((2, 2)));
    assert_eq!(second.name.as_deref(), Some("Second"));

    // A closed dialog is not part of the tree.
    assert!(tree(&ui).find("hud", "confirm").is_none());
    // The window is the root; each UI root is a group under it.
    assert_eq!(tree(&ui).root().role, AccessRole::Window);
    assert_eq!(tree(&ui).root().children, vec![AccessId::root_group("hud")]);
}

#[test]
fn disabled_nodes_report_the_state_and_offer_no_input_actions() {
    let mut button = at(UiNode::button("b", "Off", ""), 0.0, 0.0, 50.0, 20.0);
    button.enabled = false;
    let mut roots = [("hud".to_string(), root(vec![button]))];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let b = node(&ui, "hud", "b");
    assert!(b.states.disabled && !b.states.focusable);
    assert!(!b.actions.contains(&AccessAction::Click));
}

#[test]
fn authored_label_description_role_hidden_and_live_override_the_defaults() {
    let nodes = vec![
        at(UiNode::button("save", "Save", ""), 0.0, 0.0, 60.0, 20.0)
            .with_access_label("Save game")
            .with_access_description("Writes the current progress"),
        at(UiNode::label("toast", "Saved"), 0.0, 30.0, 60.0, 20.0)
            .with_access_role(AccessRole::Status)
            .with_access_live(AccessLive::Assertive),
        at(
            UiNode::container("holder", UiLayout::Column),
            0.0,
            60.0,
            60.0,
            40.0,
        )
        .access_hidden(),
        child_of(UiNode::label("secret", "Hidden text"), "holder"),
        at(
            UiNode::image("logo", "tex", UiImageFit::Stretch),
            0.0,
            110.0,
            20.0,
            20.0,
        )
        .with_tooltip("Studio logo", 0.5),
    ];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let save = node(&ui, "hud", "save");
    assert_eq!(save.name.as_deref(), Some("Save game"));
    assert_eq!(
        save.description.as_deref(),
        Some("Writes the current progress")
    );
    let toast = node(&ui, "hud", "toast");
    assert_eq!(
        (toast.role, toast.live),
        (AccessRole::Status, AccessLive::Assertive)
    );
    assert!(tree(&ui).find("hud", "holder").is_none());
    assert!(
        tree(&ui).find("hud", "secret").is_none(),
        "a hidden subtree is excluded"
    );
    // The tooltip names an otherwise unnamed image.
    assert_eq!(
        node(&ui, "hud", "logo").name.as_deref(),
        Some("Studio logo")
    );
}

#[test]
fn a_modal_dialog_hides_everything_beneath_it_from_the_tree() {
    let mut dialog = at(
        UiNode::dialog("confirm", "Quit?", vec!["Yes".into(), "No".into()]),
        0.0,
        0.0,
        200.0,
        100.0,
    );
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let nodes = vec![
        at(UiNode::button("under", "Under", ""), 0.0, 0.0, 60.0, 20.0),
        dialog,
        child_of(UiNode::label("message", "Really quit?"), "confirm"),
    ];
    let mut roots = [
        ("hud".to_string(), root(nodes)),
        (
            "other".to_string(),
            root(vec![at(
                UiNode::button("elsewhere", "Else", ""),
                0.0,
                0.0,
                60.0,
                20.0,
            )]),
        ),
    ];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let t = tree(&ui);
    assert!(t.find("hud", "under").is_none());
    assert!(
        t.find("other", "elsewhere").is_none(),
        "other roots are hidden too"
    );
    let dialog = t.find("hud", "confirm").expect("the dialog");
    assert_eq!(
        (dialog.role, dialog.name.as_deref()),
        (AccessRole::Dialog, Some("Quit?"))
    );
    assert!(dialog.states.modal);
    assert_eq!(t.modal, Some(dialog.id.clone()));
    assert!(t.find("hud", "message").is_some());
    // Footer buttons are synthetic children, the open dialog holds focus.
    let yes = t
        .get(&AccessId::node("hud", "confirm").with_part(AccessPart::Button(0)))
        .expect("dialog button");
    assert_eq!(
        (yes.role, yes.name.as_deref()),
        (AccessRole::Button, Some("Yes"))
    );
    assert_eq!(t.focus, dialog.id);
    assert!(dialog.states.focused);

    // Closing it brings the content back on the next tick.
    let events = tick(&mut ui, &mut roots, &idle());
    assert!(events.is_empty());
    ui.request_access(UiAccessRequest {
        target: AccessId::node("hud", "confirm").with_part(AccessPart::Button(1)),
        action: UiAccessAction::Click,
    })
    .unwrap();
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].name.as_str(), events[0].value.clone()),
        ("ui.dialog_result", json!(1))
    );
    tick(&mut ui, &mut roots, &idle());
    assert!(tree(&ui).find("hud", "under").is_some());
    assert!(tree(&ui).find("other", "elsewhere").is_some());
}

#[test]
fn only_the_selected_tab_panel_is_in_the_tree_and_tabs_are_synthetic_nodes() {
    let nodes = vec![
        at(
            UiNode::tabs("tabs", vec!["One".into(), "Two".into(), "Three".into()]),
            0.0,
            0.0,
            300.0,
            100.0,
        ),
        child_of(UiNode::container("p0", UiLayout::Column), "tabs"),
        child_of(UiNode::label("p0text", "Panel one"), "p0"),
        child_of(UiNode::container("p1", UiLayout::Column), "tabs"),
        child_of(UiNode::container("p2", UiLayout::Column), "tabs"),
    ];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let t = tree(&ui);
    assert!(t.find("hud", "p0").is_some());
    assert!(t.find("hud", "p1").is_none() && t.find("hud", "p2").is_none());
    let tabs = t.find("hud", "tabs").unwrap();
    assert_eq!(tabs.role, AccessRole::TabList);
    let tab = |i: u32| {
        t.get(&AccessId::node("hud", "tabs").with_part(AccessPart::Tab(i)))
            .unwrap()
            .clone()
    };
    assert_eq!(
        (tab(0).role, tab(0).name.as_deref(), tab(0).states.selected),
        (AccessRole::Tab, Some("One"), Some(true))
    );
    assert_eq!(tab(1).states.selected, Some(false));
    assert_eq!(tab(2).position, Some((3, 3)));
    let panel = t.find("hud", "p0").unwrap();
    assert_eq!(
        (panel.role, panel.name.as_deref()),
        (AccessRole::TabPanel, Some("One"))
    );
    assert_eq!(
        t.find("hud", "p0text").unwrap().name.as_deref(),
        Some("Panel one")
    );

    // Selecting the third tab swaps the panel.
    ui.request_access(UiAccessRequest {
        target: AccessId::node("hud", "tabs").with_part(AccessPart::Tab(2)),
        action: UiAccessAction::Click,
    })
    .unwrap();
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(
        (events[0].name.as_str(), events[0].value.clone()),
        ("ui.tab_changed", json!(2))
    );
    tick(&mut ui, &mut roots, &idle());
    let t = tree(&ui);
    assert!(t.find("hud", "p2").is_some());
    assert!(t.find("hud", "p0").is_none());
}

#[test]
fn an_open_dropdown_exposes_its_options_and_collapse_closes_it() {
    let nodes = vec![at(
        UiNode::dropdown(
            "quality",
            vec!["Low".into(), "Mid".into(), "High".into()],
            Some(1),
        ),
        10.0,
        10.0,
        100.0,
        20.0,
    )];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "quality", UiAccessAction::Expand);
    tick(&mut ui, &mut roots, &idle());
    tick(&mut ui, &mut roots, &idle());
    let quality = node(&ui, "hud", "quality").clone();
    assert_eq!(quality.states.expanded, Some(true));
    assert!(quality.actions.contains(&AccessAction::Collapse));
    let list = tree(&ui)
        .get(&AccessId::node("hud", "quality").with_part(AccessPart::Popup))
        .expect("option list");
    assert_eq!((list.role, list.children.len()), (AccessRole::ListBox, 3));
    let option = tree(&ui)
        .get(&AccessId::node("hud", "quality").with_part(AccessPart::Option(1)))
        .unwrap();
    assert_eq!(
        (option.role, option.name.as_deref(), option.states.selected),
        (AccessRole::ListBoxOption, Some("Mid"), Some(true))
    );
    // Choosing an option fires the same event as the popup.
    ui.request_access(UiAccessRequest {
        target: AccessId::node("hud", "quality").with_part(AccessPart::Option(2)),
        action: UiAccessAction::Click,
    })
    .unwrap();
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(
        (events[0].name.as_str(), events[0].value.clone()),
        ("ui.selection_changed", json!(2))
    );
    tick(&mut ui, &mut roots, &idle());
    assert_eq!(node(&ui, "hud", "quality").states.expanded, Some(false));
    assert!(ui.open_popup().is_none());
}

#[test]
fn virtual_list_items_get_item_scoped_ids_and_positions() {
    let mut list = at(
        UiNode::virtual_list("vl", 100, 20.0, "row"),
        0.0,
        0.0,
        200.0,
        100.0,
    );
    if let UiWidget::VirtualList { overscan, .. } = &mut list.widget {
        *overscan = 0;
    }
    let nodes = vec![
        list,
        child_of(
            UiNode::label("row", "Row").with_size(UiLength::Fill, px(20.0)),
            "vl",
        ),
    ];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let t = tree(&ui);
    let vl = t.find("hud", "vl").unwrap();
    assert_eq!((vl.role, vl.children.len()), (AccessRole::List, 5));
    let item = t.get(&AccessId::node("hud", "row").with_item(3)).unwrap();
    assert_eq!(
        (item.role, item.position),
        (AccessRole::ListItem, Some((4, 100)))
    );
    // Click on an item selects it through the same event as a pointer click.
    ui.request_access(UiAccessRequest {
        target: AccessId::node("hud", "row").with_item(3),
        action: UiAccessAction::Click,
    })
    .unwrap();
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(
        (events[0].name.as_str(), events[0].value.clone()),
        ("ui.selection_changed", json!(3))
    );
}

// ---- incremental updates -------------------------------------------------------

#[test]
fn a_diff_carries_only_the_nodes_that_changed() {
    let nodes = vec![
        at(UiNode::label("title", "Settings"), 0.0, 0.0, 100.0, 20.0),
        at(
            UiNode::slider("volume", 0.0, 0.0, 1.0, 0.25),
            0.0,
            30.0,
            100.0,
            20.0,
        ),
        at(
            UiNode::toggle("mute", "Mute", false),
            0.0,
            60.0,
            100.0,
            20.0,
        ),
    ];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    tick(&mut ui, &mut roots, &idle());
    let first = tree(&ui).clone();
    let all = first.diff(None);
    assert!(all.full);
    assert_eq!(all.nodes.len(), first.len());

    // Nothing changed: nothing is emitted.
    tick(&mut ui, &mut roots, &idle());
    let same = tree(&ui).diff(Some(&first));
    assert!(same.nodes.is_empty() && same.removed.is_empty() && !same.full);

    // One value change emits exactly that node.
    request(&mut ui, "hud", "volume", UiAccessAction::Increment);
    tick(&mut ui, &mut roots, &idle());
    let changed = tree(&ui).diff(Some(&first));
    let ids: Vec<&str> = changed.nodes.iter().map(|n| n.id.node.as_str()).collect();
    // The slider's value and, because the increment focused it, its focus state.
    assert_eq!(ids, vec!["volume"], "only the slider changed: {ids:?}");
    assert_eq!(
        changed.nodes[0].value,
        Some(AccessValue::Number {
            value: 0.25,
            min: 0.0,
            max: 1.0,
            step: 0.25
        })
    );
    assert!(changed.removed.is_empty());
    assert_eq!(changed.focus, AccessId::node("hud", "volume"));

    // Hiding a node removes it and changes its parent group.
    let before = tree(&ui).clone();
    roots[0].1.tree.nodes[2].visible = false;
    tick(&mut ui, &mut roots, &idle());
    let hidden = tree(&ui).diff(Some(&before));
    assert_eq!(hidden.removed, vec![AccessId::node("hud", "mute")]);
    assert!(
        hidden
            .nodes
            .iter()
            .any(|n| n.id == AccessId::root_group("hud"))
    );
}

#[test]
fn node_ids_are_stable_and_distinct() {
    let a = AccessId::node("hud", "play");
    assert_eq!(a.numeric(), AccessId::node("hud", "play").numeric());
    assert_ne!(a.numeric(), AccessId::node("hud", "pause").numeric());
    assert_ne!(a.numeric(), AccessId::node("hu", "dplay").numeric());
    assert_ne!(a.numeric(), a.clone().with_item(0).numeric());
    assert_ne!(
        a.numeric(),
        a.clone().with_part(AccessPart::Tab(0)).numeric()
    );
    assert_eq!(AccessId::window().numeric(), 1);
    assert!(a.numeric() > 1);
}

// ---- action requests --------------------------------------------------------------

/// The event a real pointer click on the node produces.
fn pointer_event(nodes: Vec<UiNode>, at_point: [f32; 2]) -> UiWidgetEvent {
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = GameUi::default();
    let mut events = tick(&mut ui, &mut roots, &press("ui_click", at_point));
    assert_eq!(events.len(), 1, "{events:?}");
    events.remove(0)
}

#[test]
fn click_fires_the_same_event_as_a_pointer_press() {
    let make = || {
        vec![at(
            UiNode::button("play", "Play", "start"),
            10.0,
            10.0,
            80.0,
            20.0,
        )]
    };
    let expected = pointer_event(make(), [20.0, 15.0]);
    assert_eq!(expected.name, "ui.clicked");

    let mut roots = [("hud".to_string(), root(make()))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "play", UiAccessAction::Click);
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(events, vec![expected]);
    let outcomes = ui.take_access_outcomes();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, UiAccessStatus::Applied);
    assert_eq!(ui.focus().map(|f| f.node.as_str()), Some("play"));
}

#[test]
fn toggle_click_and_slider_actions_match_pointer_and_keyboard_input() {
    let make = || {
        vec![
            at(UiNode::toggle("mute", "Mute", false), 0.0, 0.0, 80.0, 20.0),
            at(
                UiNode::slider("volume", 0.0, 0.0, 1.0, 0.25),
                0.0,
                30.0,
                100.0,
                20.0,
            ),
        ]
    };
    let mut roots = [("hud".to_string(), root(make()))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "mute", UiAccessAction::Click);
    request(&mut ui, "hud", "volume", UiAccessAction::Increment);
    request(&mut ui, "hud", "volume", UiAccessAction::Increment);
    request(&mut ui, "hud", "volume", UiAccessAction::Decrement);
    let events = tick(&mut ui, &mut roots, &idle());
    let summary: Vec<(String, String, Value)> = events
        .iter()
        .map(|e| (e.name.clone(), e.node.clone(), e.value.clone()))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("ui.value_changed".into(), "mute".into(), json!(true)),
            ("ui.value_changed".into(), "volume".into(), json!(0.25)),
            ("ui.value_changed".into(), "volume".into(), json!(0.5)),
            ("ui.value_changed".into(), "volume".into(), json!(0.25)),
        ]
    );
    // The same step through the keyboard path.
    let mut keyboard = [("hud".to_string(), root(make()))];
    let mut kui = GameUi::default();
    tick(&mut kui, &mut keyboard, &press("ui_next", [0.0; 2]));
    tick(&mut kui, &mut keyboard, &press("ui_next", [0.0; 2]));
    assert_eq!(kui.focus().map(|f| f.node.as_str()), Some("volume"));
    let key_events = tick(&mut kui, &mut keyboard, &press("ui_right", [0.0; 2]));
    assert_eq!(key_events.len(), 1);
    assert_eq!(key_events[0].value, json!(0.25));
    assert_eq!(key_events[0].name, events[1].name);
    assert_eq!(key_events[0].action, events[1].action);
}

#[test]
fn set_numeric_quantizes_and_rejects_out_of_range_values() {
    let nodes = vec![at(
        UiNode::slider("volume", 0.0, 0.0, 1.0, 0.25),
        0.0,
        0.0,
        100.0,
        20.0,
    )];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "volume", UiAccessAction::SetNumeric(0.6));
    request(&mut ui, "hud", "volume", UiAccessAction::SetNumeric(7.0));
    request(
        &mut ui,
        "hud",
        "volume",
        UiAccessAction::SetText("x".into()),
    );
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value, json!(0.5));
    let status: Vec<UiAccessStatus> = ui
        .take_access_outcomes()
        .into_iter()
        .map(|o| o.status)
        .collect();
    assert_eq!(
        status,
        vec![
            UiAccessStatus::Applied,
            UiAccessStatus::Rejected,
            UiAccessStatus::Unsupported
        ]
    );
    assert!(
        ui.request_access(UiAccessRequest {
            target: AccessId::node("hud", "volume"),
            action: UiAccessAction::SetNumeric(f64::NAN),
        })
        .is_err()
    );
}

#[test]
fn set_text_fires_the_event_typing_fires_and_enforces_the_limits() {
    let make = || {
        let mut input = at(UiNode::text_input("name", "Name", 8), 0.0, 0.0, 120.0, 20.0);
        input.style.font = "font".into();
        vec![input]
    };
    // Typing: focus by click, then queue an insert.
    let mut typed = [("hud".to_string(), root(make()))];
    let mut tui = GameUi::default();
    tick(&mut tui, &mut typed, &press("ui_click", [5.0, 5.0]));
    tui.push_text_event(UiTextEvent::Insert("Ada".into()))
        .unwrap();
    let typed_events = tick(&mut tui, &mut typed, &idle());
    assert_eq!(typed_events.len(), 1);

    let mut roots = [("hud".to_string(), root(make()))];
    let mut ui = ui_with_tree();
    request(
        &mut ui,
        "hud",
        "name",
        UiAccessAction::SetText("Ada".into()),
    );
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(events, typed_events, "same event as typing");
    let UiWidget::TextInput {
        text,
        caret,
        anchor,
        ..
    } = &roots[0].1.tree.nodes[0].widget
    else {
        unreachable!()
    };
    assert_eq!((text.as_str(), *caret, *anchor), ("Ada", 3, 3));
    tick(&mut ui, &mut roots, &idle());
    assert_eq!(
        node(&ui, "hud", "name").value,
        Some(AccessValue::Text("Ada".into()))
    );
    // Too long (max_length 8) and control characters are rejected unchanged.
    request(
        &mut ui,
        "hud",
        "name",
        UiAccessAction::SetText("123456789".into()),
    );
    request(
        &mut ui,
        "hud",
        "name",
        UiAccessAction::SetText("a\u{7}b".into()),
    );
    let events = tick(&mut ui, &mut roots, &idle());
    assert!(events.is_empty());
    let status: Vec<_> = ui
        .take_access_outcomes()
        .into_iter()
        .map(|o| o.status)
        .collect();
    assert_eq!(
        &status[1..],
        &[UiAccessStatus::Rejected, UiAccessStatus::Rejected]
    );
    let UiWidget::TextInput { text, .. } = &roots[0].1.tree.nodes[0].widget else {
        unreachable!()
    };
    assert_eq!(text, "Ada");
}

#[test]
fn requests_under_a_modal_or_for_missing_or_disabled_nodes_are_not_applied() {
    let mut dialog = at(
        UiNode::dialog("confirm", "Quit?", vec!["Yes".into()]),
        0.0,
        0.0,
        200.0,
        100.0,
    );
    if let UiWidget::ModalDialog { open, .. } = &mut dialog.widget {
        *open = true;
    }
    let mut off = at(UiNode::button("off", "Off", ""), 0.0, 100.0, 50.0, 20.0);
    off.enabled = false;
    let nodes = vec![
        at(
            UiNode::button("under", "Under", "x"),
            0.0,
            200.0,
            60.0,
            20.0,
        ),
        off,
        dialog,
    ];
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "under", UiAccessAction::Click);
    request(&mut ui, "hud", "off", UiAccessAction::Click);
    request(&mut ui, "hud", "nothing", UiAccessAction::Click);
    request(&mut ui, "ghost", "under", UiAccessAction::Click);
    let events = tick(&mut ui, &mut roots, &idle());
    assert!(events.is_empty());
    let status: Vec<_> = ui
        .take_access_outcomes()
        .into_iter()
        .map(|o| o.status)
        .collect();
    assert_eq!(
        status,
        vec![
            UiAccessStatus::Blocked, // beneath the modal
            UiAccessStatus::Blocked, // disabled
            UiAccessStatus::NotFound,
            UiAccessStatus::NotFound,
        ]
    );
}

#[test]
fn the_request_queue_is_bounded() {
    let mut ui = ui_with_tree();
    for _ in 0..MAX_ACCESS_REQUESTS {
        request(&mut ui, "hud", "x", UiAccessAction::Focus);
    }
    let overflow = ui
        .request_access(UiAccessRequest {
            target: AccessId::node("hud", "x"),
            action: UiAccessAction::Focus,
        })
        .unwrap_err();
    assert_eq!(overflow.diagnostics[0].code, "ui.access.overflow");
    let long = ui
        .request_access(UiAccessRequest {
            target: AccessId::node("hud", "x".repeat(300)),
            action: UiAccessAction::Focus,
        })
        .unwrap_err();
    assert_eq!(long.diagnostics[0].code, "ui.access.invalid");
}

#[test]
fn scroll_into_view_scrolls_the_ancestors_and_keyboard_focus_follows() {
    let mut view = at(
        UiNode::scroll_view("view", UiScrollAxes::Vertical),
        0.0,
        0.0,
        100.0,
        100.0,
    );
    view.style.layout = UiLayout::Column;
    if let UiWidget::ScrollView { scrollbars, .. } = &mut view.widget {
        *scrollbars = false;
    }
    let mut nodes = vec![view];
    for i in 0..10 {
        nodes.push(child_of(
            UiNode::button(format!("b{i}"), "", "").with_size(UiLength::Fill, px(40.0)),
            "view",
        ));
    }
    let mut roots = [("hud".to_string(), root(nodes))];
    let mut ui = ui_with_tree();
    request(&mut ui, "hud", "b9", UiAccessAction::ScrollIntoView);
    let events = tick(&mut ui, &mut roots, &idle());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "ui.scrolled");
    let UiWidget::ScrollView { offset, .. } = &roots[0].1.tree.nodes[0].widget else {
        unreachable!()
    };
    // b9 spans y 360..400 of 400 total in a 100 px view: scrolled to the end.
    assert_eq!(offset[1], 300.0);

    // Keyboard navigation scrolls the newly focused widget into view.
    let mut keyboard = [("hud".to_string(), {
        let mut r = roots[0].1.clone();
        if let UiWidget::ScrollView { offset, .. } = &mut r.tree.nodes[0].widget {
            *offset = [0.0, 0.0];
        }
        r
    })];
    let mut kui = GameUi::default();
    for _ in 0..4 {
        tick(&mut kui, &mut keyboard, &press("ui_next", [0.0; 2]));
    }
    assert_eq!(kui.focus().map(|f| f.node.as_str()), Some("b3"));
    let UiWidget::ScrollView { offset, .. } = &keyboard[0].1.tree.nodes[0].widget else {
        unreachable!()
    };
    // b3 spans 120..160: the view now ends at its bottom edge.
    assert_eq!(offset[1], 60.0);
}

// ---- settings: UI scale, text scale, reduced motion, themes -------------------------

fn runtime(entities: Value) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(&json!({
            "schema":1,"name":"a11y","startup_scene":"scene.json","assets":[],
            "inputBindings":{"jump":["Key:Space"],"use":["Key:E"],"fire":["Mouse:Left"]}
        }))
        .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    (dir, runtime)
}

fn ui_entity(id: &str, root: &UiRoot) -> Value {
    json!({"id":id,"name":id,"components":{"custom":{"engine.UiRoot":{"version":1,"data":root}}}})
}

fn control(runtime: &mut Runtime) -> bevy_ecs::prelude::Mut<'_, AccessibilityControl> {
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<AccessibilityControl>()
}

fn clicks(runtime: &Runtime) -> Vec<String> {
    runtime
        .app()
        .world()
        .resource::<UiTickEvents>()
        .0
        .iter()
        .filter(|e| e.name == "ui.clicked")
        .map(|e| e.node.clone())
        .collect()
}

fn chrome_x_range(runtime: &Runtime) -> (f32, f32, f32, f32) {
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id.starts_with("game-ui:") && d.id.ends_with(":0"))
        .expect("chrome draw");
    let xs: Vec<f32> = draw.mesh.vertices.iter().map(|v| v.position[0]).collect();
    let ys: Vec<f32> = draw.mesh.vertices.iter().map(|v| v.position[1]).collect();
    (
        xs.iter().copied().fold(f32::MAX, f32::min),
        xs.iter().copied().fold(f32::MIN, f32::max),
        ys.iter().copied().fold(f32::MAX, f32::min),
        ys.iter().copied().fold(f32::MIN, f32::max),
    )
}

#[test]
fn ui_scale_scales_layout_drawing_and_hit_testing_consistently() {
    let button = at(UiNode::button("b", "", "go"), 100.0, 50.0, 100.0, 40.0);
    let hud = ui_entity("hud", &root(vec![button]));
    let (_dir, mut runtime) = runtime(json!([hud]));
    // Unscaled: the button covers x 100..200, y 50..90.
    runtime
        .tick(0.016, press("ui_click", [110.0, 60.0]))
        .unwrap();
    assert_eq!(clicks(&runtime), vec!["b"]);
    assert_eq!(chrome_x_range(&runtime), (100.0, 200.0, 50.0, 90.0));

    control(&mut runtime).set_ui_scale(1.5).unwrap();
    // Scaled by 1.5: x 150..300, y 75..135. The old rectangle's point misses...
    runtime.tick(0.016, idle()).unwrap();
    runtime
        .tick(0.016, press("ui_click", [110.0, 60.0]))
        .unwrap();
    assert!(clicks(&runtime).is_empty(), "{:?}", clicks(&runtime));
    // ...and a point inside the scaled one hits, at its edges too.
    runtime.tick(0.016, idle()).unwrap();
    runtime
        .tick(0.016, press("ui_click", [151.0, 76.0]))
        .unwrap();
    assert_eq!(clicks(&runtime), vec!["b"]);
    runtime.tick(0.016, idle()).unwrap();
    runtime
        .tick(0.016, press("ui_click", [299.0, 134.0]))
        .unwrap();
    assert_eq!(clicks(&runtime), vec!["b"]);
    runtime.tick(0.016, idle()).unwrap();
    runtime
        .tick(0.016, press("ui_click", [301.0, 134.0]))
        .unwrap();
    assert!(clicks(&runtime).is_empty());
    assert_eq!(chrome_x_range(&runtime), (150.0, 300.0, 75.0, 135.0));
    // The accessibility bounds are in window pixels as well.
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<GameUi>()
        .set_accessibility(true);
    runtime.tick(0.016, idle()).unwrap();
    let ui = runtime.app().world().resource::<GameUi>();
    assert_eq!(ui.scale(), 1.5);
    let bounds = node(ui, "hud", "b").bounds;
    assert_eq!(
        (bounds.x, bounds.y, bounds.width, bounds.height),
        (150.0, 75.0, 150.0, 60.0)
    );
}

#[test]
fn ui_scale_layout_viewport_covers_the_window() {
    let scaling = UiScaling {
        ui_scale: 1.5,
        ..Default::default()
    };
    assert_eq!(scaling.layout_viewport([1280, 720]), [854, 480]);
    assert_eq!(
        UiScaling::default().layout_viewport([1280, 720]),
        [1280, 720]
    );
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
fn text_scale_multiplies_font_scale_and_the_measured_text_height() {
    let (fonts, textures) = font_assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let mut label = UiNode::label("l", "AAAA").with_style(|s| {
        s.font = "font".into();
        s.padding = 0.0;
        s.width = UiLength::Auto;
        s.height = UiLength::Auto;
    });
    label.style.font_scale = 1.0;
    let base = root(vec![label]);
    let measure = |root: &UiRoot| {
        let boxes = root.tree.layout_with(VIEWPORT, &backend).unwrap();
        (boxes[0].rect.width, boxes[0].rect.height)
    };
    assert_eq!(measure(&base), (20.0, 8.0));

    let mut doubled = base.clone();
    UiScaling {
        text_scale: 2.0,
        ..Default::default()
    }
    .apply_text(&mut doubled, &backend);
    assert_eq!(doubled.tree.nodes[0].style.font_scale, 2.0);
    assert_eq!(measure(&doubled), (40.0, 16.0));
    assert_eq!(
        base.tree.nodes[0].style.font_scale, 1.0,
        "authored data is untouched"
    );

    // The floor raises small text: 24 px lines at UI scale 1 need fontScale 3.
    let mut floor = base.clone();
    UiScaling {
        min_text_size: 24.0,
        ..Default::default()
    }
    .apply_text(&mut floor, &backend);
    assert_eq!(floor.tree.nodes[0].style.font_scale, 3.0);
    assert_eq!(measure(&floor).1, 24.0);
    // The floor is on-screen pixels: at UI scale 2 half the scale suffices.
    let mut scaled_ui = base.clone();
    UiScaling {
        ui_scale: 2.0,
        min_text_size: 24.0,
        ..Default::default()
    }
    .apply_text(&mut scaled_ui, &backend);
    assert_eq!(scaled_ui.tree.nodes[0].style.font_scale, 1.5);
    // Text scale never leaves the schema range.
    let mut huge = base;
    huge.tree.nodes[0].style.font_scale = 8.0;
    UiScaling {
        text_scale: 4.0,
        ..Default::default()
    }
    .apply_text(&mut huge, &backend);
    assert_eq!(huge.tree.nodes[0].style.font_scale, 16.0);
    assert!(huge.validate().is_ok());
}

/// A bare world with one root and the accessibility resource.
fn bare_world(root: UiRoot, settings: AccessibilitySettings) -> World {
    let mut world = World::new();
    world.spawn((
        root,
        AuthoredId {
            id: "hud".into(),
            name: "hud".into(),
        },
    ));
    world.insert_resource(AccessibilityControl {
        settings,
        ..Default::default()
    });
    world.insert_resource(GameUi::default());
    world.insert_resource(UiStyleSettings::default());
    world
}

#[test]
fn text_scale_and_ui_scale_reach_the_tick_layout_and_the_drawn_glyphs() {
    let (fonts, textures) = font_assets();
    let backend = BitmapTextBackend::new(&fonts, &textures);
    let label = UiNode::label("l", "AAAA").with_style(|s| {
        s.font = "font".into();
        s.padding = 0.0;
        s.width = UiLength::Auto;
        s.height = UiLength::Auto;
    });
    let mut results = vec![];
    for (ui_scale, text_scale) in [(1.0, 1.0), (1.0, 2.0), (2.0, 1.0), (2.0, 2.0)] {
        let settings = AccessibilitySettings {
            ui_scale,
            text_scale,
            screen_reader: true,
            ..Default::default()
        };
        let mut world = bare_world(root(vec![label.clone()]), settings);
        tick_world(
            &mut world,
            &UiTickContext {
                viewport: VIEWPORT,
                dt: 0.016,
                text: &backend,
            },
            &idle(),
        )
        .unwrap();
        let ui = world.resource::<GameUi>();
        let bounds = node(ui, "hud", "l").bounds;
        let draws = extract_world(&world, VIEWPORT, &fonts, &textures).unwrap();
        let glyph = draws
            .iter()
            .find(|d| d.texture.is_some())
            .expect("a glyph draw");
        let ys: Vec<f32> = glyph.mesh.vertices.iter().map(|v| v.position[1]).collect();
        let xs: Vec<f32> = glyph.mesh.vertices.iter().map(|v| v.position[0]).collect();
        let span = |v: &[f32]| {
            v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min)
        };
        results.push(((bounds.width, bounds.height), (span(&xs), span(&ys))));
    }
    // Four 5 px advances, one 8 px line: scales multiply, window pixels out.
    assert_eq!(results[0].0, (20.0, 8.0));
    assert_eq!(
        results[1].0,
        (40.0, 16.0),
        "text scale doubles the measured text"
    );
    assert_eq!(
        results[2].0,
        (40.0, 16.0),
        "UI scale doubles the window-pixel bounds"
    );
    assert_eq!(results[3].0, (80.0, 32.0));
    // The drawn glyph quads agree with the layout (a quad of the last glyph
    // spans 4x8 at scale 1 inside a 20 px box).
    assert_eq!(results[0].1.1, 8.0);
    assert_eq!(results[1].1.1, 16.0);
    assert_eq!(results[2].1.1, 16.0);
    assert_eq!(results[3].1.1, 32.0);
}

#[test]
fn reduced_motion_and_the_high_contrast_theme_reach_the_style_settings() {
    let hud = ui_entity(
        "hud",
        &root(vec![at(UiNode::button("b", "", ""), 0.0, 0.0, 50.0, 20.0)]),
    );
    let (_dir, mut runtime) = runtime(json!([hud]));
    {
        let world = runtime.app_mut().world_mut();
        let mut style = world.resource_mut::<UiStyleSettings>();
        style
            .register_theme(
                serde_json::from_value(json!({"version":1,"name":"plain","rules":[]})).unwrap(),
            )
            .unwrap();
        style
            .register_theme(
                serde_json::from_value(json!({"version":1,"name":"contrast","rules":[
                    {"selector":"button","style":{"background":[0,0,0,1]}}]}))
                .unwrap(),
            )
            .unwrap();
        style.set_active_theme("plain");
    }
    runtime.tick(0.016, idle()).unwrap();
    assert!(
        !runtime
            .app()
            .world()
            .resource::<UiStyleSettings>()
            .reduced_motion()
    );

    control(&mut runtime).set_reduced_motion(true);
    control(&mut runtime).settings.high_contrast_theme = Some("contrast".into());
    runtime.tick(0.016, idle()).unwrap();
    let style = runtime.app().world().resource::<UiStyleSettings>();
    assert!(style.reduced_motion());
    assert_eq!(style.active_theme(), Some("contrast"));

    control(&mut runtime).set_reduced_motion(false);
    control(&mut runtime).settings.high_contrast_theme = None;
    runtime.tick(0.016, idle()).unwrap();
    let style = runtime.app().world().resource::<UiStyleSettings>();
    assert!(!style.reduced_motion());
    assert_eq!(
        style.active_theme(),
        Some("plain"),
        "the previous theme returns"
    );
}

#[test]
fn the_authored_component_drives_the_resource() {
    let settings = json!({"version":1,"uiScale":1.25,"textScale":1.5,"reducedMotion":true,
        "screenReader":true,"focusThickness":4,"focusHighContrast":true});
    let (_dir, mut runtime) = runtime(json!([
        {"id":"prefs","name":"prefs","components":{"custom":{
            "engine.AccessibilitySettings":{"version":1,"data":settings}}}}
    ]));
    runtime.tick(0.016, idle()).unwrap();
    let world = runtime.app().world();
    let effective = &world.resource::<AccessibilityControl>().settings;
    assert_eq!(
        (
            effective.ui_scale,
            effective.text_scale,
            effective.focus_thickness
        ),
        (1.25, 1.5, 4.0)
    );
    assert!(effective.reduced_motion && effective.focus_high_contrast);
    assert!(world.resource::<UiStyleSettings>().reduced_motion());
    // The screen-reader hint turns the tree on.
    assert!(world.resource::<GameUi>().accessibility_enabled());
    // A snapshot carries the component, so it round-trips through scene JSON.
    let snapshot = runtime.try_snapshot().unwrap();
    let prefs = snapshot
        .scene
        .entities
        .iter()
        .find(|e| e.id == "prefs")
        .unwrap();
    assert_eq!(
        prefs.components.custom["engine.AccessibilitySettings"].data["uiScale"],
        json!(1.25)
    );
}

#[test]
fn the_focus_ring_follows_the_thickness_and_contrast_options() {
    let mut b = at(UiNode::button("b", "", ""), 0.0, 0.0, 50.0, 20.0);
    b.nav.tab_index = 0;
    let hud = ui_entity("hud", &root(vec![b]));
    let (_dir, mut runtime) = runtime(json!([hud]));
    let ring_quads = |runtime: &Runtime| {
        let frame = runtime.extract_frame().unwrap();
        let draw = frame
            .draws
            .iter()
            .find(|d| d.id.starts_with("game-ui:") && d.id.ends_with(":0"))
            .unwrap();
        draw.mesh.vertices.len() / 4
    };
    runtime.tick(0.016, press("ui_next", [0.0; 2])).unwrap();
    // Background quad plus four ring edges.
    assert_eq!(ring_quads(&runtime), 5);
    {
        let mut c = control(&mut runtime);
        c.settings.focus_high_contrast = true;
        c.settings.focus_thickness = 3.0;
    }
    runtime.tick(0.016, idle()).unwrap();
    assert_eq!(ring_quads(&runtime), 9, "two rings");
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id.starts_with("game-ui:") && d.id.ends_with(":0"))
        .unwrap();
    // The top edge of the outer ring is 3 px tall and white.
    let quad = &draw.mesh.vertices[4..8];
    assert_eq!(quad[0].color, [1.0; 4]);
    let heights: Vec<f32> = quad.iter().map(|v| v.position[1]).collect();
    assert_eq!(
        heights.iter().cloned().fold(f32::MIN, f32::max)
            - heights.iter().cloned().fold(f32::MAX, f32::min),
        3.0
    );
}

#[test]
fn the_bridge_status_names_why_it_is_unavailable() {
    let control = AccessibilityControl::default();
    match &control.bridge {
        AccessBridgeStatus::Unavailable(reason) => assert!(reason.contains("no platform")),
        other => panic!("{other:?}"),
    }
}

// ---- descriptors ------------------------------------------------------------------

fn catalog() -> ComponentCatalog {
    ComponentCatalog::new(vec![]).unwrap()
}

fn validate(name: &str, data: Value) -> Result<()> {
    catalog().validate(name, &CustomValue { version: 1, data })
}

#[test]
fn the_settings_descriptor_accepts_samples_and_rejects_invalid_input() {
    assert!(catalog().0.contains_key("engine.AccessibilitySettings"));
    for good in [
        json!({"version":1}),
        json!({"version":1,"uiScale":2,"textScale":0.75,"minTextSize":18,"reducedMotion":true,
               "screenReader":false,"focusThickness":3,"focusHighContrast":true,
               "highContrastTheme":"high-contrast"}),
    ] {
        validate("engine.AccessibilitySettings", good.clone())
            .unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        json!({"version":1,"uiScale":0.1}),
        json!({"version":1,"uiScale":9}),
        json!({"version":1,"textScale":0}),
        json!({"version":1,"minTextSize":500}),
        json!({"version":1,"focusThickness":0}),
        json!({"version":1,"reducedMotion":"yes"}),
        json!({"version":1,"highContrastTheme":""}),
        json!({"version":1,"highContrastTheme":"not valid!"}),
        json!({"version":2}),
        json!({"version":1,"unknown":true}),
    ] {
        assert!(
            validate("engine.AccessibilitySettings", bad.clone()).is_err(),
            "{bad} should be rejected"
        );
    }
    let mut settings = AccessibilitySettings::default();
    assert!(settings.validate().is_ok());
    settings.ui_scale = f32::NAN;
    assert!(settings.validate().is_err());
}

#[test]
fn the_ui_root_descriptor_validates_the_accessibility_field() {
    let node = |accessibility: Value| {
        json!({"version":1,"tree":{"nodes":[
            {"id":"a","widget":{"kind":"button","text":"Go"},"accessibility":accessibility}
        ]}})
    };
    for good in [
        json!({"label":"Go now"}),
        json!({"label":"Go","description":"Starts the game","role":"link","live":"polite"}),
        json!({"hidden":true}),
    ] {
        validate("engine.UiRoot", node(good.clone())).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    for bad in [
        json!({"role":"window"}),
        json!({"role":"made_up"}),
        json!({"live":"loud"}),
        json!({"label":"x".repeat(300)}),
        json!({"description":"x".repeat(1100)}),
        json!({"label":"a\u{7}b"}),
        json!({"unknown":1}),
        json!({"hidden":"yes"}),
    ] {
        assert!(
            validate("engine.UiRoot", node(bad.clone())).is_err(),
            "{bad}"
        );
    }
    // Existing version-1 scenes without the field still load unchanged.
    let plain: UiRoot = serde_json::from_value(json!({"version":1,"tree":{"nodes":[
        {"id":"a","widget":{"kind":"button","text":"Go"}}]}}))
    .unwrap();
    assert!(plain.tree.nodes[0].accessibility.is_none());
    let back = serde_json::to_value(&plain).unwrap();
    assert!(back["tree"]["nodes"][0].get("accessibility").is_none());
}

// ---- the remapping screen ---------------------------------------------------------

fn bindings(pairs: &[(&str, &[&str])]) -> BindingMap {
    pairs
        .iter()
        .map(|(a, t)| (a.to_string(), t.iter().map(|s| s.to_string()).collect()))
        .collect()
}

fn defaults() -> BindingMap {
    bindings(&[
        ("fire", &["Mouse:Left"]),
        ("jump", &["Key:Space", "Gamepad:A"]),
        ("use", &["Key:E"]),
    ])
}

fn screen(conflicts: RemapConflicts) -> RemapScreen {
    RemapScreen::new(
        defaults(),
        defaults(),
        RemapOptions {
            conflicts,
            ..Default::default()
        },
    )
    .unwrap()
}

fn label_text(root: &UiRoot, id: &str) -> String {
    match &root.tree.nodes[root.tree.find(id).unwrap()].widget {
        UiWidget::Label { text } | UiWidget::Button { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_remap_screen_lists_every_action_with_its_bindings() {
    let screen = screen(RemapConflicts::Warn);
    let root = screen.root().unwrap();
    for action in ["fire", "jump", "use"] {
        assert_eq!(label_text(&root, &format!("remap.{action}.name")), action);
    }
    assert_eq!(label_text(&root, "remap.jump.slot.0"), "Space");
    assert_eq!(label_text(&root, "remap.jump.slot.1"), "Gamepad A");
    assert_eq!(label_text(&root, "remap.fire.slot.0"), "Mouse Left");
    assert!(root.tree.find("remap.reset_all").is_some());
    // Rows are accessible: names state the action, the slot and the binding.
    let slot = &root.tree.nodes[root.tree.find("remap.jump.slot.0").unwrap()];
    assert_eq!(
        slot.accessibility.as_ref().unwrap().label.as_deref(),
        Some("jump, binding 1: Space. Activate to change")
    );
    // Too many actions are refused instead of overflowing the node budget.
    let many: BindingMap = (0..60)
        .map(|i| (format!("a{i}"), vec!["Key:A".to_string()]))
        .collect();
    assert!(RemapScreen::new(many.clone(), many, RemapOptions::default()).is_err());
}

#[test]
fn capturing_a_key_rebinds_and_shows_the_conflict() {
    let mut screen = screen(RemapConflicts::Warn);
    assert_eq!(
        screen.press("remap.jump.slot.0"),
        vec![RemapEvent::CaptureStarted {
            action: "jump".into()
        }]
    );
    assert!(
        screen
            .message()
            .starts_with("Press a key or button for jump")
    );
    let root = screen.root().unwrap();
    assert_eq!(label_text(&root, "remap.jump.slot.0"), "Press a key...");
    // Key:E is already used by `use`.
    let events = screen.capture_token("Key:E");
    assert_eq!(
        events,
        vec![
            RemapEvent::Rebound {
                action: "jump".into(),
                previous: Some("Key:Space".into()),
                token: "Key:E".into(),
                bindings: vec!["Key:E".into(), "Gamepad:A".into()],
            },
            RemapEvent::Conflict {
                token: "Key:E".into(),
                actions: vec!["use".into()],
                applied: true,
            },
        ]
    );
    assert_eq!(screen.bindings()["jump"], vec!["Key:E", "Gamepad:A"]);
    assert_eq!(screen.conflicts()["Key:E"], vec!["jump", "use"]);
    assert!(screen.capturing().is_none());
    let root = screen.root().unwrap();
    let conflict = &root.tree.nodes[root.tree.find("remap.jump.conflict").unwrap()];
    assert!(conflict.visible);
    assert_eq!(
        label_text(&root, "remap.jump.conflict"),
        "Conflict: E also used by use"
    );
    assert!(root.tree.nodes[root.tree.find("remap.use.conflict").unwrap()].visible);
    assert!(!root.tree.nodes[root.tree.find("remap.fire.conflict").unwrap()].visible);
    assert!(screen.message().contains("also used by use"));

    // Reset restores the default and clears the conflict.
    let events = screen.press("remap.jump.reset");
    assert!(matches!(&events[0], RemapEvent::Reset { action, .. } if action == "jump"));
    assert_eq!(screen.bindings()["jump"], vec!["Key:Space", "Gamepad:A"]);
    assert!(screen.conflicts().is_empty());
    assert!(
        !screen.root().unwrap().tree.nodes[screen
            .root()
            .unwrap()
            .tree
            .find("remap.jump.conflict")
            .unwrap()]
        .visible
    );
}

#[test]
fn escape_cancels_unknown_tokens_are_refused_and_add_appends_a_binding() {
    let mut screen = screen(RemapConflicts::Warn);
    screen.press("remap.use.slot.0");
    assert_eq!(
        screen.capture_token("Key:Escape"),
        vec![RemapEvent::CaptureCancelled {
            action: "use".into()
        }]
    );
    assert_eq!(screen.bindings()["use"], vec!["Key:E"]);
    screen.press("remap.use.slot.0");
    let refused = screen.capture_token("Gamepad:Bogus");
    assert!(matches!(&refused[0], RemapEvent::Rejected { .. }));
    assert!(screen.capturing().is_none());
    screen.press("remap.use.add");
    let events = screen.capture_token("Key:F");
    assert!(matches!(
        &events[0],
        RemapEvent::Rebound { previous: None, .. }
    ));
    assert_eq!(screen.bindings()["use"], vec!["Key:E", "Key:F"]);
    // Binding a key the action already has is refused without a change.
    screen.press("remap.use.add");
    let again = screen.capture_token("Key:F");
    assert!(matches!(&again[0], RemapEvent::Rejected { .. }));
    assert_eq!(screen.bindings()["use"], vec!["Key:E", "Key:F"]);
    // Outside a capture, tokens do nothing.
    assert!(screen.capture_token("Key:G").is_empty());
    // Reset all returns to the defaults.
    assert_eq!(screen.press("remap.reset_all"), vec![RemapEvent::ResetAll]);
    assert_eq!(screen.bindings(), &defaults());
}

#[test]
fn the_reject_policy_keeps_the_old_binding_and_names_the_owner() {
    let mut screen = screen(RemapConflicts::Reject);
    screen.press("remap.jump.slot.0");
    let events = screen.capture_token("Key:E");
    assert!(matches!(
        &events[0],
        RemapEvent::Conflict { applied: false, actions, .. } if actions == &vec!["use".to_string()]
    ));
    assert!(matches!(&events[1], RemapEvent::Rejected { .. }));
    assert_eq!(screen.bindings()["jump"], vec!["Key:Space", "Gamepad:A"]);
    assert!(screen.message().contains("already used by use"));
}

#[test]
fn the_remap_screen_is_keyboard_navigable_and_activation_starts_a_capture() {
    let screen = screen(RemapConflicts::Warn);
    let mut roots = [("remap".to_string(), screen.root().unwrap())];
    let mut ui = ui_with_tree();
    let viewport = [800, 600];
    // Tab walks the buttons in reading order: the first row's first slot.
    ui.tick(&mut roots, viewport, &press("ui_next", [0.0; 2]))
        .unwrap();
    assert_eq!(
        ui.focus().map(|f| f.node.as_str()),
        Some("remap.fire.slot.0")
    );
    ui.tick(&mut roots, viewport, &press("ui_next", [0.0; 2]))
        .unwrap();
    assert_eq!(ui.focus().map(|f| f.node.as_str()), Some("remap.fire.add"));
    ui.tick(&mut roots, viewport, &press("ui_previous", [0.0; 2]))
        .unwrap();
    let events = ui
        .tick(&mut roots, viewport, &press("ui_confirm", [0.0; 2]))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].name.as_str(), events[0].node.as_str()),
        ("ui.clicked", "remap.fire.slot.0")
    );
    // The accessibility tree names the controls and marks the status line live.
    let t = tree(&ui);
    let slot = t.find("remap", "remap.fire.slot.0").unwrap();
    assert_eq!(slot.role, AccessRole::Button);
    assert!(
        slot.name
            .as_deref()
            .unwrap()
            .contains("fire, binding 1: Mouse Left")
    );
    let status = t.find("remap", "remap.status").unwrap();
    assert_eq!(
        (status.role, status.live),
        (AccessRole::Status, AccessLive::Polite)
    );
    assert_eq!(t.find("remap", "remap").unwrap().role, AccessRole::Region);
    assert_eq!(
        t.find("remap", "remap.title").unwrap().role,
        AccessRole::Heading
    );
}

#[derive(Resource, Default)]
struct Observed(Vec<Value>);

fn audit(world: &mut World, _owner: Entity, event: &EngineEvent) -> Result<()> {
    world.resource_mut::<Observed>().0.push(json!({
        "name": event.name, "payload": event.payload.clone()
    }));
    Ok(())
}

fn controller_world() -> (tempfile::TempDir, Runtime, Entity) {
    let (dir, mut runtime) = runtime(json!([{"id":"menu","name":"menu","components":{
        "EventBindings":{"bindings":[
            {"event":"input.rebound","handler":"audit"},
            {"event":"input.rebind_conflict","handler":"audit"}]}}}]));
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Observed::default());
    engine_project::register_event_handler(runtime.app_mut(), "audit", audit).unwrap();
    let entity = runtime.entity("menu").unwrap();
    (dir, runtime, entity)
}

fn set_tick(world: &mut World, clicked: &[&str], down: &[&str]) {
    world.insert_resource(UiTickEvents(
        clicked
            .iter()
            .map(|node| UiWidgetEvent {
                root: "menu".into(),
                node: (*node).into(),
                name: "ui.clicked".into(),
                action: "remap.rebind".into(),
                value: Value::Null,
            })
            .collect(),
    ));
    world.insert_resource(RawInputTokens {
        down: down.iter().map(|s| s.to_string()).collect(),
    });
}

#[test]
fn the_controller_applies_a_captured_key_to_the_input_settings_and_emits_events() {
    let (_dir, mut runtime, entity) = controller_world();
    let world = runtime.app_mut().world_mut();
    let mut controller = RemapController::new(world, entity, RemapOptions::default()).unwrap();
    // The scene's `use` and `fire` keep their bindings.
    assert_eq!(
        world.resource::<InputSettings>().bindings["use"],
        vec!["Key:E"]
    );
    assert_eq!(controller.root_key(), "menu");
    // The UI exists on the entity.
    assert!(world.get::<UiRoot>(entity).is_some());
    let revision = world.resource::<InputSettings>().revision;

    // The press that starts the capture (the mouse is down) does not bind itself.
    set_tick(world, &["remap.jump.slot.0"], &["Mouse:Left"]);
    let events = controller.tick(world).unwrap();
    assert_eq!(
        events,
        vec![RemapEvent::CaptureStarted {
            action: "jump".into()
        }]
    );
    assert_eq!(world.resource::<InputSettings>().revision, revision);

    // Still held: nothing new. Then a real key goes down.
    set_tick(world, &[], &["Mouse:Left"]);
    assert!(controller.tick(world).unwrap().is_empty());
    set_tick(world, &[], &["Mouse:Left", "Key:Q"]);
    let events = controller.tick(world).unwrap();
    assert!(matches!(&events[0], RemapEvent::Rebound { token, .. } if token == "Key:Q"));
    let settings = world.resource::<InputSettings>();
    assert_eq!(settings.bindings["jump"], vec!["Key:Q"]);
    assert_eq!(
        settings.revision,
        revision + 1,
        "the physical mapper is told to rebind"
    );
    // The shown UI updated too.
    let root = world.get::<UiRoot>(entity).unwrap();
    assert_eq!(label_text(root, "remap.jump.slot.0"), "Q");
    // The next runtime tick delivers the engine event to its handler.
    runtime.tick(0.016, idle()).unwrap();
    let observed = &runtime.app().world().resource::<Observed>().0;
    assert_eq!(observed.len(), 1, "{observed:?}");
    assert_eq!(observed[0]["name"], "input.rebound");
    assert_eq!(observed[0]["payload"]["token"], "Key:Q");
    assert_eq!(observed[0]["payload"]["action"], "jump");
}
