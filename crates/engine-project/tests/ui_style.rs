#![recursion_limit = "512"]
//! Stylesheets, themes, state selectors, transitions, keyframes, reduced
//! motion, data binding, world-space panels, descriptors and serialized size.
use bevy_ecs::prelude::World;
use engine_project::{
    ActionFrame, ComponentCatalog, CustomValue, Project, Runtime,
    content::{Mesh, Vertex},
    runtime::{Draw, DrawSpace},
    ui_tree::*,
};
use glam::{Mat4, Vec3};
use serde_json::{Value, json};
use std::collections::BTreeMap;

// ---- helpers ---------------------------------------------------------------

fn theme(value: Value) -> UiTheme {
    serde_json::from_value(value).unwrap()
}

fn node(id: &str, widget: UiWidget) -> UiNode {
    UiNode::new(id, widget).with_style(|s| s.padding = 0.0)
}

fn child(mut n: UiNode, parent: &str) -> UiNode {
    n.parent = Some(parent.into());
    n
}

fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}

fn button(id: &str) -> UiNode {
    node(
        id,
        UiWidget::Button {
            text: String::new(),
            action: "go".into(),
        },
    )
}

fn style_of<'a>(root: &'a UiRoot, id: &str) -> &'a UiStyle {
    &root.tree.nodes[root.tree.find(id).unwrap()].style
}

fn no_states() -> BTreeMap<String, UiStateFlags> {
    BTreeMap::new()
}

fn resolve(root: &UiRoot, theme: &UiTheme, states: &BTreeMap<String, UiStateFlags>) -> UiRoot {
    let composed = UiComposedTheme::new(theme).unwrap();
    UiStyleRuntime::default()
        .resolve("hud", root, &composed, states)
        .unwrap()
}

fn close(a: [f32; 4], b: [f32; 4]) {
    assert!(
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5),
        "{a:?} != {b:?}"
    );
}

fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-4, "{a} != {b}");
}

fn press(action: &str, pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pressed: [action.into()].into(),
        pointer,
        ..Default::default()
    }
}

fn at(pointer: [f32; 2]) -> ActionFrame {
    ActionFrame {
        pointer,
        ..Default::default()
    }
}

fn runtime(entities: Value) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"ui-style","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    (dir, runtime)
}

fn entity(id: &str, components: Value) -> Value {
    json!({"id":id,"name":id,"components":components})
}

fn ui_entity(id: &str, root: &UiRoot) -> Value {
    entity(
        id,
        json!({"custom":{"engine.UiRoot":{"version":1,"data":root}}}),
    )
}

fn theme_entity(id: &str, theme: Value) -> Value {
    entity(
        id,
        json!({"custom":{"engine.UiTheme":{"version":1,"data":theme}}}),
    )
}

fn world(runtime: &Runtime) -> &World {
    runtime.app().world()
}

fn root_of(runtime: &Runtime, id: &str) -> UiRoot {
    let snapshot = runtime.try_snapshot().unwrap();
    let entity = snapshot.scene.entities.iter().find(|e| e.id == id).unwrap();
    serde_json::from_value(entity.components.custom["engine.UiRoot"].data.clone()).unwrap()
}

/// First chrome draw (background quad) of the first screen-space UI root.
fn background_color(runtime: &Runtime) -> [f32; 4] {
    let frame = runtime.extract_frame().unwrap();
    let draw = frame
        .draws
        .iter()
        .find(|d| d.id.starts_with("game-ui:") && d.id.ends_with(":0"))
        .expect("a chrome draw");
    draw.mesh.vertices[0].color
}

// ---- cascade ---------------------------------------------------------------

fn cascade_theme() -> UiTheme {
    theme(json!({
        "version":1,"name":"t",
        "tokens":{"colors":{"red":[1,0,0,1],"green":[0,1,0,1],"blue":[0,0,1,1],"yellow":[1,1,0,1]},
                  "numbers":{"pad":10,"wide":200}},
        "rules":[
            {"selector":"button","style":{"background":"$red","padding":"$pad"}},
            {"selector":".primary","style":{"background":"$green","accent":"$green"}},
            {"selector":"button.primary","style":{"background":"$yellow"}},
            {"selector":"#ok","style":{"background":"$blue","width":"$wide"}},
            {"selector":".x","style":{"foreground":[1,0,0,1]}},
            {"selector":".y","style":{"foreground":[0,1,0,1]}},
            {"selector":"button","style":{"accent":"$blue"}}
        ]
    }))
}

fn cascade_root() -> UiRoot {
    let classes = |n: UiNode, c: &[&str]| {
        let mut n = n;
        n.classes = c.iter().map(|s| s.to_string()).collect();
        n
    };
    let mut inline = button("inline");
    inline.style.background = [0.5, 0.5, 0.5, 1.0];
    // Default inline styles (`UiNode::button`): rules, not the node, decide.
    let styled = |id: &str| UiNode::button(id, "", "go");
    root(vec![
        styled("plain"),
        classes(styled("primary"), &["primary"]),
        classes(styled("ok"), &["primary"]),
        classes(
            node("label", UiWidget::Label { text: "x".into() }),
            &["primary"],
        ),
        classes(button("xy"), &["x", "y"]),
        inline,
    ])
}

#[test]
fn specificity_source_order_partial_override_and_inline_resolve_the_expected_colors() {
    let resolved = resolve(&cascade_root(), &cascade_theme(), &no_states());
    // kind only
    close(
        style_of(&resolved, "plain").background,
        [1.0, 0.0, 0.0, 1.0],
    );
    assert_eq!(style_of(&resolved, "plain").padding, 10.0);
    // kind + class (0,1,1) beats class (0,1,0) even though its rule comes later
    // in the file than `.primary`, and unset fields keep the lower rule's value
    let primary = style_of(&resolved, "primary");
    close(primary.background, [1.0, 1.0, 0.0, 1.0]);
    assert_eq!(primary.padding, 10.0, "padding inherited from `button`");
    // `.primary` (0,1,0) sets accent; the later `button` accent rule (0,0,1) loses
    close(primary.accent, [0.0, 1.0, 0.0, 1.0]);
    close(style_of(&resolved, "plain").accent, [0.0, 0.0, 1.0, 1.0]);
    // id (1,0,0) beats everything; a token works as a pixel length
    let ok = style_of(&resolved, "ok");
    close(ok.background, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(ok.width, UiLength::Pixels(200.0));
    // a label only matches `.primary`
    let label = style_of(&resolved, "label");
    close(label.background, [0.0, 1.0, 0.0, 1.0]);
    assert_eq!(label.padding, 0.0, "the inline padding of the node");
    // equal specificity: the later rule (`.y`) wins
    close(style_of(&resolved, "xy").foreground, [0.0, 1.0, 0.0, 1.0]);
    // inline fields that differ from the default beat every rule
    close(
        style_of(&resolved, "inline").background,
        [0.5, 0.5, 0.5, 1.0],
    );
}

#[test]
fn descendant_and_child_combinators_and_text_inheritance() {
    let sheet = theme(json!({
        "version":1,"name":"c",
        "rules":[
            {"selector":".menu button","style":{"background":[0,1,0,1]}},
            {"selector":".menu > .row > label","style":{"background":[0,0,1,1]}},
            {"selector":".menu","style":{"foreground":[1,0,0,1],"fontScale":2}},
            {"selector":"label, toggle","style":{"opacity":0.5}}
        ]
    }));
    let mut menu = node("menu", UiWidget::Container);
    menu.classes = vec!["menu".into()];
    let mut row = child(node("row", UiWidget::Container), "menu");
    row.classes = vec!["row".into()];
    let tree = root(vec![
        menu,
        row,
        child(button("deep"), "row"),
        child(node("lab", UiWidget::Label { text: "a".into() }), "row"),
        child(node("direct", UiWidget::Label { text: "b".into() }), "menu"),
        button("outside"),
    ]);
    let resolved = resolve(&tree, &sheet, &no_states());
    close(style_of(&resolved, "deep").background, [0.0, 1.0, 0.0, 1.0]);
    close(style_of(&resolved, "lab").background, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(
        style_of(&resolved, "direct").background,
        UiStyle::DEFAULT.background,
        "`direct` is a child of .menu, not of .row"
    );
    assert_ne!(
        style_of(&resolved, "outside").background,
        [0.0, 1.0, 0.0, 1.0]
    );
    // foreground and font scale inherit down the tree
    close(style_of(&resolved, "deep").foreground, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(style_of(&resolved, "lab").font_scale, 2.0);
    assert_eq!(style_of(&resolved, "lab").opacity, 0.5, "selector list");
    assert_eq!(style_of(&resolved, "outside").font_scale, 1.0);
}

#[test]
fn theme_switch_changes_resolved_styles_and_extends_layers_over_the_base() {
    let mut set = UiThemeSet::new();
    set.insert(theme(json!({
        "version":1,"name":"base",
        "tokens":{"colors":{"surface":[1,1,1,1],"ink":[0,0,0,1]}},
        "rules":[{"selector":"button","style":{"background":"$surface","foreground":"$ink","padding":6}}]
    })))
    .unwrap();
    set.insert(theme(json!({
        "version":1,"name":"dark","extends":"base",
        "tokens":{"colors":{"surface":[0,0,0,1]}},
        "rules":[{"selector":"button:hover","style":{"accent":[1,0,0,1]}}]
    })))
    .unwrap();
    let tree = root(vec![UiNode::button("b", "", "go")]);
    let runtime = UiStyleRuntime::default();
    let light = runtime
        .resolve("hud", &tree, &set.compose("base").unwrap(), &no_states())
        .unwrap();
    let dark = runtime
        .resolve("hud", &tree, &set.compose("dark").unwrap(), &no_states())
        .unwrap();
    close(style_of(&light, "b").background, [1.0; 4]);
    close(style_of(&dark, "b").background, [0.0, 0.0, 0.0, 1.0]);
    // the derived theme keeps the base rules and tokens it does not override
    close(style_of(&dark, "b").foreground, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(style_of(&dark, "b").padding, 6.0);
    assert!(set.compose("missing").is_err());
}

#[test]
fn theme_switch_at_runtime_through_settings_and_the_authored_root_field() {
    let sheet = |name: &str, color: [f32; 4]| {
        theme_entity(
            name,
            json!({"version":1,"name":name,"rules":[{"selector":"button","style":{"background":color}}]}),
        )
    };
    let hud = ui_entity("hud", &root(vec![button("b")]));
    let (_dir, mut runtime) = runtime(json!([
        hud,
        sheet("light", [1.0, 1.0, 1.0, 1.0]),
        sheet("dark", [0.0, 0.0, 0.0, 1.0])
    ]));
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiStyleSettings>()
        .set_active_theme("light");
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    close(background_color(&runtime), [1.0; 4]);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiStyleSettings>()
        .set_active_theme("dark");
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    close(background_color(&runtime), [0.0, 0.0, 0.0, 1.0]);
    // An unknown theme is a stable error, never silently unstyled.
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiStyleSettings>()
        .set_active_theme("nope");
    let error = runtime.tick(0.016, ActionFrame::default()).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "ui.theme.unknown");
}

// ---- state selectors -------------------------------------------------------

fn states_of(ui: &GameUi, root: &str) -> BTreeMap<String, UiStateFlags> {
    ui.states()
        .iter()
        .filter(|(k, _)| k.root == root)
        .map(|(k, v)| (k.node.clone(), *v))
        .collect()
}

fn state_theme() -> UiTheme {
    theme(json!({
        "version":1,"name":"s",
        "rules":[
            {"selector":"button","style":{"background":[0.5,0.5,0.5,1]}},
            {"selector":"button:hover","style":{"background":[0,1,0,1]}},
            {"selector":"button:pressed","style":{"background":[0,0,1,1]}},
            {"selector":"button:focus","style":{"accent":[1,0,0,1]}},
            {"selector":"button:disabled","style":{"opacity":0.25}},
            {"selector":"button:enabled","style":{"scale":1.5}},
            {"selector":"toggle:checked","style":{"background":[1,1,0,1]}}
        ]
    }))
}

#[test]
fn hover_pressed_focused_disabled_and_checked_rules_follow_gameui_state() {
    let mut panel = node("panel", UiWidget::Container);
    panel.style.layout = UiLayout::Column;
    panel.style.gap = 4.0;
    let mut off = child(button("off"), "panel");
    off.enabled = false;
    let tree = root(vec![
        panel,
        child(button("b1"), "panel"),
        child(button("b2"), "panel"),
        off,
        child(
            node(
                "check",
                UiWidget::Toggle {
                    text: String::new(),
                    checked: true,
                    action: String::new(),
                },
            ),
            "panel",
        ),
    ]);
    let boxes = tree.tree.layout([800, 600]).unwrap();
    let centre = |id: &str| {
        let b = boxes
            .iter()
            .find(|b| tree.tree.nodes[b.node].id == id)
            .unwrap()
            .rect;
        [b.x + b.width / 2.0, b.y + b.height / 2.0]
    };
    let sheet = state_theme();
    let mut roots = vec![("hud".to_string(), tree.clone())];
    let mut ui = GameUi::default();

    ui.tick(&mut roots, [800, 600], &at(centre("b1"))).unwrap();
    let resolved = resolve(&roots[0].1, &sheet, &states_of(&ui, "hud"));
    close(style_of(&resolved, "b1").background, [0.0, 1.0, 0.0, 1.0]);
    close(style_of(&resolved, "b2").background, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(style_of(&resolved, "b1").scale, 1.5, ":enabled");
    assert_eq!(style_of(&resolved, "off").opacity, 0.25, ":disabled");
    assert_eq!(style_of(&resolved, "off").scale, 1.0, "not :enabled");
    close(
        style_of(&resolved, "check").background,
        [1.0, 1.0, 0.0, 1.0],
    );

    ui.tick(&mut roots, [800, 600], &press("ui_click", centre("b1")))
        .unwrap();
    assert!(ui.state("hud", "b1").pressed());
    let resolved = resolve(&roots[0].1, &sheet, &states_of(&ui, "hud"));
    close(style_of(&resolved, "b1").background, [0.0, 0.0, 1.0, 1.0]);

    ui.tick(&mut roots, [800, 600], &ActionFrame::default())
        .unwrap();
    ui.tick(&mut roots, [800, 600], &press("ui_next", [-1.0, -1.0]))
        .unwrap();
    let focused = ui.focus().unwrap().node.clone();
    assert!(ui.state("hud", &focused).focused());
    let resolved = resolve(&roots[0].1, &sheet, &states_of(&ui, "hud"));
    close(style_of(&resolved, &focused).accent, [1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn runtime_hover_rule_changes_the_drawn_background_after_the_pointer_enters() {
    let hud = ui_entity("hud", &root(vec![button("b")]));
    let (_dir, mut runtime) = runtime(json!([
        hud,
        theme_entity(
            "theme",
            json!({"version":1,"name":"main","rules":[
                {"selector":"button","style":{"background":[0.5,0.5,0.5,1]}},
                {"selector":"button:hover","style":{"background":[0,1,0,1]}}
            ]})
        )
    ]));
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiStyleSettings>()
        .set_active_theme("main");
    runtime.tick(0.016, at([200.0, 150.0])).unwrap();
    close(background_color(&runtime), [0.5, 0.5, 0.5, 1.0]);
    runtime.tick(0.016, at([5.0, 5.0])).unwrap();
    close(background_color(&runtime), [0.0, 1.0, 0.0, 1.0]);
    runtime.tick(0.016, at([200.0, 150.0])).unwrap();
    close(background_color(&runtime), [0.5, 0.5, 0.5, 1.0]);
}

// ---- transitions, keyframes, reduced motion --------------------------------

#[test]
fn easing_functions_hit_their_known_values() {
    assert_eq!(Easing::Linear.apply(0.25), 0.25);
    assert_eq!(Easing::EaseIn.apply(0.5), 0.25);
    assert_eq!(Easing::EaseOut.apply(0.5), 0.75);
    assert_eq!(Easing::EaseInOut.apply(0.25), 0.125);
    assert_eq!(Easing::CubicIn.apply(0.5), 0.125);
    assert_eq!(Easing::CubicOut.apply(0.5), 0.875);
    assert_eq!(Easing::Steps(4).apply(0.6), 0.5);
    for easing in [
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::CubicInOut,
        Easing::Bezier([0.25, 0.1, 0.25, 1.0]),
    ] {
        near(easing.apply(0.0), 0.0);
        near(easing.apply(1.0), 1.0);
    }
    // cubic-bezier(0.42, 0, 0.58, 1) is symmetric: 0.5 maps to 0.5
    near(Easing::Bezier([0.42, 0.0, 0.58, 1.0]).apply(0.5), 0.5);
    assert_eq!(Easing::Linear.apply(f32::NAN), 0.0);
}

#[test]
fn a_transition_interpolates_a_color_at_zero_half_and_full_with_its_easing() {
    let transition = UiTransition::new(UiAnimProperty::Background, 2.0, Easing::EaseIn);
    let (from, to) = (
        UiValue::Color([0.0, 0.0, 0.0, 1.0]),
        UiValue::Color([1.0; 4]),
    );
    let at = |t: f32| match transition.sample(from, to, t) {
        UiValue::Color(c) => c,
        other => panic!("{other:?}"),
    };
    close(at(0.0), [0.0, 0.0, 0.0, 1.0]);
    close(at(1.0), [0.25, 0.25, 0.25, 1.0]); // 50% of 2 s, eased: 0.5^2
    close(at(2.0), [1.0; 4]);
    close(at(5.0), [1.0; 4]);
    let delayed = UiTransition {
        delay: 1.0,
        ..transition
    };
    assert_eq!(delayed.sample(from, to, 0.9), from);
    assert_eq!(delayed.sample(from, to, 3.0), to);
}

fn motion_theme() -> UiTheme {
    theme(json!({
        "version":1,"name":"m",
        "rules":[
            {"selector":"button","style":{"background":[0,0,0,1]},
             "transition":[{"property":"background","duration":1.0,"easing":"ease_in"},
                           {"property":"opacity","duration":2.0}]},
            {"selector":"button:hover","style":{"background":[1,1,1,1],"opacity":0.5}}
        ]
    }))
}

fn hover() -> BTreeMap<String, UiStateFlags> {
    BTreeMap::from([("b".to_string(), UiStateFlags::HOVERED)])
}

#[test]
fn a_state_transition_steps_deterministically_through_the_eased_curve() {
    let sheet = UiComposedTheme::new(&motion_theme()).unwrap();
    let tree = root(vec![button("b")]);
    let mut runtime = UiStyleRuntime::default();
    let bg = |r: &UiRoot| style_of(r, "b").background;
    let opacity = |r: &UiRoot| style_of(r, "b").opacity;
    let step = |runtime: &mut UiStyleRuntime, states: &BTreeMap<String, UiStateFlags>, dt: f32| {
        runtime
            .advance("hud", &tree, &sheet, states, dt, false)
            .unwrap()
    };
    // first sight: no transition
    let r = step(&mut runtime, &no_states(), 0.0);
    close(bg(&r), [0.0, 0.0, 0.0, 1.0]);
    // the state flips: the target changes, the shown value is still the start (t = 0)
    let r = step(&mut runtime, &hover(), 0.0);
    close(bg(&r), [0.0, 0.0, 0.0, 1.0]);
    assert!(runtime.is_animating("hud"));
    // 50% of 1 s with ease_in: 0.5^2
    let r = step(&mut runtime, &hover(), 0.5);
    close(bg(&r), [0.25, 0.25, 0.25, 1.0]);
    near(opacity(&r), 0.875); // 25% of 2 s, linear
    // full duration
    let r = step(&mut runtime, &hover(), 0.5);
    close(bg(&r), [1.0; 4]);
    near(opacity(&r), 0.75); // 1.0 of 2.0 s, linear: halfway to 0.5
    let r = step(&mut runtime, &hover(), 1.0);
    near(opacity(&r), 0.5);
    assert!(!runtime.is_animating("hud"));
    // leaving the state transitions back (the transition lives on the base rule)
    let r = step(&mut runtime, &no_states(), 0.0);
    close(bg(&r), [1.0; 4]);
    let r = step(&mut runtime, &no_states(), 1.0);
    close(bg(&r), [0.0, 0.0, 0.0, 1.0]);
    // `resolve` is read-only: it shows the current motion without advancing it
    let before = bg(&step(&mut runtime, &hover(), 0.0));
    let peeked = runtime.resolve("hud", &tree, &sheet, &hover()).unwrap();
    assert_eq!(bg(&peeked), before);
}

#[test]
fn reduced_motion_makes_transitions_instant_and_drops_non_essential_animation() {
    let sheet = UiComposedTheme::new(&motion_theme()).unwrap();
    let tree = root(vec![button("b")]);
    let mut runtime = UiStyleRuntime::default();
    runtime
        .advance("hud", &tree, &sheet, &no_states(), 0.0, true)
        .unwrap();
    let r = runtime
        .advance("hud", &tree, &sheet, &hover(), 0.0, true)
        .unwrap();
    close(style_of(&r, "b").background, [1.0; 4]);
    near(style_of(&r, "b").opacity, 0.5);
    assert!(!runtime.is_animating("hud"));

    let pulse = |essential: bool| {
        theme(json!({
            "version":1,"name":"p",
            "keyframes":{"pulse":{"frames":[{"at":0,"opacity":1},{"at":0.5,"opacity":0.5},{"at":1,"opacity":1}]}},
            "rules":[{"selector":"button","animation":
                {"name":"pulse","duration":1.0,"iterations":"infinite","essential":essential}}]
        }))
    };
    for (essential, reduced, expected) in
        [(false, false, 0.75), (false, true, 1.0), (true, true, 0.75)]
    {
        let sheet = UiComposedTheme::new(&pulse(essential)).unwrap();
        let mut runtime = UiStyleRuntime::default();
        runtime
            .advance("hud", &tree, &sheet, &no_states(), 0.0, reduced)
            .unwrap();
        let r = runtime
            .advance("hud", &tree, &sheet, &no_states(), 0.25, reduced)
            .unwrap();
        near(style_of(&r, "b").opacity, expected);
    }
}

#[test]
fn keyframe_animation_loops_wraps_alternates_and_holds() {
    let looping = UiAnimation::looping("pulse", 1.0);
    let progress = |a: &UiAnimation, t: f64| match a.phase(t) {
        UiAnimationPhase::Running(p) | UiAnimationPhase::Finished(p) => p,
        UiAnimationPhase::Waiting => -1.0,
    };
    near(progress(&looping, 0.25), 0.25);
    near(progress(&looping, 1.25), 0.25);
    near(progress(&looping, 3.75), 0.75);
    near(progress(&looping, 2.0), 0.0);
    let alternate = UiAnimation {
        direction: UiAnimationDirection::Alternate,
        ..looping.clone()
    };
    near(progress(&alternate, 0.25), 0.25);
    near(progress(&alternate, 1.25), 0.75);
    near(progress(&alternate, 2.25), 0.25);
    let once = UiAnimation {
        name: "pulse".into(),
        duration: 1.0,
        delay: 0.5,
        ..Default::default()
    };
    assert_eq!(once.phase(0.25), UiAnimationPhase::Waiting);
    assert_eq!(once.phase(1.0), UiAnimationPhase::Running(0.5));
    assert_eq!(once.phase(2.0), UiAnimationPhase::Finished(1.0));
    let twice = UiAnimation {
        iterations: UiIterations::Count(2),
        direction: UiAnimationDirection::Alternate,
        ..once
    };
    assert_eq!(twice.phase(10.0), UiAnimationPhase::Finished(0.0));

    // a node-level loop: opacity 1 -> 0.5 -> 1, sampled across the wrap
    let sheet = UiComposedTheme::new(&theme(json!({
        "version":1,"name":"p",
        "keyframes":{"pulse":{"frames":[{"at":0,"opacity":1},{"at":0.5,"opacity":0.5},{"at":1,"opacity":1}]}},
        "rules":[{"selector":"button","animation":{"name":"pulse","duration":1.0,"iterations":"infinite"}}]
    })))
    .unwrap();
    let tree = root(vec![button("b")]);
    let mut runtime = UiStyleRuntime::default();
    let mut opacity_after = |dt: f32| {
        let r = runtime
            .advance("hud", &tree, &sheet, &no_states(), dt, false)
            .unwrap();
        style_of(&r, "b").opacity
    };
    near(opacity_after(0.0), 1.0);
    near(opacity_after(0.25), 0.75);
    near(opacity_after(0.25), 0.5);
    near(opacity_after(0.5), 1.0); // t = 1.0 wraps to the start
    near(opacity_after(0.25), 0.75); // t = 1.25
    // Keyframes that omit the endpoints start and end at the base value.
    let keys = UiKeyframes {
        frames: vec![UiKeyframe {
            at: 0.5,
            opacity: Some(0.0),
            ..Default::default()
        }],
    };
    let base = |p: UiAnimProperty| (p == UiAnimProperty::Opacity).then_some(UiValue::Number(1.0));
    assert_eq!(
        keys.sample(0.25, &base),
        vec![(UiAnimProperty::Opacity, UiValue::Number(0.5))]
    );
    assert_eq!(
        keys.sample(1.0, &base),
        vec![(UiAnimProperty::Opacity, UiValue::Number(1.0))]
    );
}

#[test]
fn runtime_transition_plays_over_ticks_and_reduced_motion_setting_skips_it() {
    for reduced in [false, true] {
        let hud = ui_entity("hud", &root(vec![button("b")]));
        let (_dir, mut runtime) = runtime(json!([
            hud,
            theme_entity(
                "theme",
                json!({"version":1,"name":"main","rules":[
                    {"selector":"button","style":{"background":[0,0,0,1]},
                     "transition":[{"property":"background","duration":1.0}]},
                    {"selector":"button:hover","style":{"background":[1,1,1,1]}}
                ]})
            )
        ]));
        {
            let world = runtime.app_mut().world_mut();
            let mut settings = world.resource_mut::<UiStyleSettings>();
            settings.set_active_theme("main");
            settings.set_reduced_motion(reduced);
        }
        runtime.tick(0.25, at([200.0, 150.0])).unwrap();
        close(background_color(&runtime), [0.0, 0.0, 0.0, 1.0]);
        runtime.tick(0.25, at([5.0, 5.0])).unwrap();
        let r = background_color(&runtime)[0];
        if reduced {
            near(r, 1.0);
            continue;
        }
        near(r, 0.0); // the hover began this tick: t = 0
        let mut seen = vec![];
        for _ in 0..4 {
            runtime.tick(0.25, at([5.0, 5.0])).unwrap();
            seen.push(background_color(&runtime)[0]);
        }
        for (got, want) in seen.iter().zip([0.25, 0.5, 0.75, 1.0]) {
            near(*got, want);
        }
    }
}

#[test]
fn opacity_and_scale_reach_the_draws_and_never_move_layout() {
    let mut plain = node("a", UiWidget::Container);
    plain.style.width = UiLength::Pixels(40.0);
    plain.style.height = UiLength::Pixels(20.0);
    let mut scaled = plain.clone();
    scaled.style.scale = 2.0;
    scaled.style.opacity = 0.5;
    scaled.id = "b".into();
    for (n, x) in [(&mut plain, 100.0), (&mut scaled, 100.0)] {
        n.style.offset = [x, 50.0];
    }
    let mut inner = child(node("c", UiWidget::Container), "b");
    inner.style.width = UiLength::Pixels(10.0);
    inner.style.height = UiLength::Pixels(10.0);
    let tree = root(vec![plain, scaled, inner]);
    let draws = |tree: &UiRoot| -> Vec<Draw> {
        tree.extract_draws_with(
            "hud",
            [320, 180],
            &UiRenderState::default(),
            &NullTextBackend,
            &BTreeMap::new(),
        )
        .unwrap()
    };
    let out = draws(&tree);
    let first = |draw: &Draw| draw.mesh.vertices[0].clone();
    // node a (index 0), b (1), c (2): chrome draws only (no text)
    let a = first(&out[0]);
    assert_eq!(a.position[0], 100.0);
    assert_eq!(a.color[3], 1.0);
    // b is 40x20 at (100,50), scaled 2x about its centre (120, 60): x 80..160
    let b = first(&out[1]);
    assert_eq!([b.position[0], b.position[1]], [80.0, 40.0]);
    near(b.color[3], 0.5);
    // c inherits b's scale and opacity: its own box is at (100,50) 10x10
    let c = first(&out[2]);
    assert_eq!([c.position[0], c.position[1]], [80.0, 40.0]);
    near(c.color[3], 0.5);
    // layout is untouched by paint scale
    let boxes = tree.tree.layout([320, 180]).unwrap();
    assert_eq!(boxes[1].rect.width, 40.0);
    // opacity 0 draws nothing
    let mut hidden = tree.clone();
    hidden.tree.nodes[1].style.opacity = 0.0;
    assert_eq!(draws(&hidden).len(), 1);
}

// ---- binding ---------------------------------------------------------------

fn binding_root() -> UiRoot {
    let mut panel = node("panel", UiWidget::Container);
    panel.style.layout = UiLayout::Column;
    panel.style.gap = 4.0;
    panel.style.width = UiLength::Fill;
    panel.style.height = UiLength::Fill;
    let mut list = child(
        node(
            "inv",
            UiWidget::VirtualList {
                item_count: 0,
                item_extent: 20.0,
                template: "row".into(),
                scroll: 0.0,
                selected: None,
                overscan: 1,
                horizontal: false,
                action: String::new(),
            },
        ),
        "panel",
    );
    list.style.height = UiLength::Pixels(60.0);
    let mut root = root(vec![
        panel,
        child(
            node(
                "hp",
                UiWidget::Label {
                    text: String::new(),
                },
            ),
            "panel",
        ),
        child(
            node(
                "vol",
                UiWidget::Slider {
                    value: 0.25,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    action: String::new(),
                },
            ),
            "panel",
        ),
        child(
            node(
                "mute",
                UiWidget::Toggle {
                    text: String::new(),
                    checked: false,
                    action: String::new(),
                },
            ),
            "panel",
        ),
        list,
        child(node("row", UiWidget::Container), "inv"),
        child(
            node(
                "rowtext",
                UiWidget::Label {
                    text: String::new(),
                },
            ),
            "row",
        ),
    ]);
    root.bindings = vec![
        UiBinding::one_way("hp", UiBindProperty::Text, "player.hp")
            .with_format("HP {}/{player.max:.0}"),
        UiBinding::two_way("vol", UiBindProperty::Value, "audio.volume"),
        UiBinding::two_way("mute", UiBindProperty::Checked, "audio.mute"),
        UiBinding::one_way("inv", UiBindProperty::ItemCount, "items"),
        UiBinding::one_way("rowtext", UiBindProperty::ItemText, "items")
            .with_item_path("name")
            .with_format("{#}:{}"),
    ];
    root
}

fn data(runtime: &mut Runtime) -> bevy_ecs::prelude::Mut<'_, UiDataContext> {
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
}

fn stats(runtime: &Runtime) -> UiBindingStats {
    world(runtime).resource::<UiBindingState>().stats()
}

fn label_text(root: &UiRoot, id: &str) -> String {
    match &root.tree.nodes[root.tree.find(id).unwrap()].widget {
        UiWidget::Label { text } => text.clone(),
        other => panic!("{other:?}"),
    }
}

fn centre_of(root: &UiRoot, viewport: [u32; 2], id: &str, fx: f32) -> [f32; 2] {
    let boxes = root.tree.layout(viewport).unwrap();
    let b = boxes
        .iter()
        .find(|b| root.tree.nodes[b.node].id == id)
        .unwrap()
        .rect;
    [b.x + b.width * fx, b.y + b.height / 2.0]
}

#[test]
fn one_way_bindings_update_label_text_and_virtual_list_count_with_change_detection() {
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &binding_root())]));
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    // data missing: nothing applied yet, no widget touched
    assert_eq!(label_text(&root_of(&runtime, "hud"), "hp"), "");

    {
        let mut ctx = data(&mut runtime);
        assert!(ctx.set("player.hp", json!(7)).unwrap());
        ctx.set("player.max", json!(10.0)).unwrap();
        ctx.set("items", json!([{"name":"A"},{"name":"B"},{"name":"C"}]))
            .unwrap();
    }
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let hud = root_of(&runtime, "hud");
    assert_eq!(label_text(&hud, "hp"), "HP 7/10");
    let UiWidget::VirtualList { item_count, .. } =
        &hud.tree.nodes[hud.tree.find("inv").unwrap()].widget
    else {
        unreachable!()
    };
    assert_eq!(*item_count, 3);

    // Unchanged frames skip evaluation entirely.
    let baseline = stats(&runtime);
    for _ in 0..5 {
        runtime.tick(0.016, ActionFrame::default()).unwrap();
    }
    let idle = stats(&runtime);
    assert_eq!(idle.applied, baseline.applied);
    assert_eq!(idle.evaluated, baseline.evaluated);
    assert_eq!(idle.frames_skipped, baseline.frames_skipped + 5);
    // Writing an identical value does not bump the version either.
    assert!(!data(&mut runtime).set("player.hp", json!(7)).unwrap());
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(stats(&runtime).applied, baseline.applied);
    // A real change re-applies only the bindings whose rendered value changed.
    data(&mut runtime).set("player.hp", json!(8)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let after = stats(&runtime);
    assert_eq!(
        after.applied,
        baseline.applied + 1,
        "only the hp text changed"
    );
    assert_eq!(after.frames_evaluated, baseline.frames_evaluated + 1);
    assert_eq!(label_text(&root_of(&runtime, "hud"), "hp"), "HP 8/10");
    // Appending an item changes the count; renaming one does not.
    data(&mut runtime)
        .set("items[3]", json!({"name":"D"}))
        .unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let hud = root_of(&runtime, "hud");
    let UiWidget::VirtualList { item_count, .. } =
        &hud.tree.nodes[hud.tree.find("inv").unwrap()].widget
    else {
        unreachable!()
    };
    assert_eq!(*item_count, 4);
    let applied = stats(&runtime).applied;
    data(&mut runtime).set("items[0].name", json!("Z")).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(
        stats(&runtime).applied,
        applied,
        "same count: nothing applied"
    );

    // Per-item text comes from the data at render time.
    let ctx = world(&runtime).resource::<UiDataContext>();
    let texts = world(&runtime)
        .resource::<UiBindingState>()
        .item_texts(&hud, ctx);
    assert_eq!(texts.text("rowtext", 0).as_deref(), Some("0:Z"));
    assert_eq!(texts.text("rowtext", 3).as_deref(), Some("3:D"));
    assert_eq!(texts.text("rowtext", 9), None);
    assert_eq!(texts.text("hp", 0), None);
}

#[test]
fn two_way_bindings_write_slider_and_toggle_changes_back_to_the_data() {
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &binding_root())]));
    data(&mut runtime).set("audio.volume", json!(0.5)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let viewport = runtime.extract_camera().logical_size;
    let hud = root_of(&runtime, "hud");
    let UiWidget::Slider { value, .. } = hud.tree.nodes[hud.tree.find("vol").unwrap()].widget
    else {
        unreachable!()
    };
    assert_eq!(value, 0.5, "the data drove the slider");

    let before = stats(&runtime).writes;
    let target = centre_of(&hud, viewport, "vol", 0.8);
    runtime.tick(0.016, press("ui_click", target)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let hud = root_of(&runtime, "hud");
    let UiWidget::Slider { value, .. } = hud.tree.nodes[hud.tree.find("vol").unwrap()].widget
    else {
        unreachable!()
    };
    assert!(
        value > 0.7 && value < 0.9,
        "dragged to about 0.8, got {value}"
    );
    let written = world(&runtime)
        .resource::<UiDataContext>()
        .get_f64("audio.volume")
        .unwrap();
    near(written as f32, value);
    assert!(stats(&runtime).writes > before);

    // Toggle: clicking writes the checked state.
    let target = centre_of(&hud, viewport, "mute", 0.1);
    runtime.tick(0.016, press("ui_click", target)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(
        world(&runtime)
            .resource::<UiDataContext>()
            .get_bool("audio.mute"),
        Some(true)
    );
    // The data drives it back.
    data(&mut runtime).set("audio.mute", json!(false)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let hud = root_of(&runtime, "hud");
    assert!(matches!(
        hud.tree.nodes[hud.tree.find("mute").unwrap()].widget,
        UiWidget::Toggle { checked: false, .. }
    ));
}

struct Shout;
impl UiStringResolver for Shout {
    fn resolve(&self, key: &str, args: &BTreeMap<String, Value>) -> Option<String> {
        (key == "hp.label").then(|| {
            format!(
                "VIE {}",
                args.get("n").and_then(Value::as_i64).unwrap_or_default()
            )
        })
    }
}

#[test]
fn message_bindings_go_through_the_string_resolver_seam_and_fall_back_without_one() {
    let mut tree = root(vec![node(
        "hp",
        UiWidget::Label {
            text: String::new(),
        },
    )]);
    tree.bindings = vec![
        UiBinding::one_way("hp", UiBindProperty::Text, "player.hp")
            .with_format("HP {}")
            .with_message("hp.label", [("n".to_string(), "player.hp".to_string())]),
    ];
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &tree)]));
    data(&mut runtime).set("player.hp", json!(4)).unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(
        label_text(&root_of(&runtime, "hud"), "hp"),
        "HP 4",
        "format fallback"
    );
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiBindingState>()
        .set_resolver(std::sync::Arc::new(Shout));
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(label_text(&root_of(&runtime, "hud"), "hp"), "VIE 4");
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiBindingState>()
        .clear_resolver();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(label_text(&root_of(&runtime, "hud"), "hp"), "HP 4");
}

#[test]
fn ui_data_components_drive_the_context_and_paths_are_bounded() {
    let mut tree = root(vec![node(
        "hp",
        UiWidget::Label {
            text: String::new(),
        },
    )]);
    tree.bindings = vec![UiBinding::one_way("hp", UiBindProperty::Text, "score")];
    let (_dir, mut runtime) = runtime(json!([
        ui_entity("hud", &tree),
        entity(
            "seed",
            json!({"custom":{"engine.UiData":{"version":1,"data":{"values":{"score":12}}}}})
        )
    ]));
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    assert_eq!(label_text(&root_of(&runtime, "hud"), "hp"), "12");

    let mut ctx = UiDataContext::new();
    ctx.set("a.b[0].c", json!(1)).unwrap();
    assert_eq!(ctx.get("a.b[0].c").unwrap(), Some(&json!(1)));
    assert!(ctx.set("a.b[5]", json!(1)).is_err(), "gap in an array");
    assert!(ctx.set("a.b.c", json!(1)).is_err(), "through an array");
    let long = "a.".repeat(40);
    for bad in ["", ".a", "a..b", "a[", "a[x]", "a b", "a[1", long.as_str()] {
        assert!(parse_path(bad).is_err(), "accepted `{bad}`");
    }
    assert!(parse_path("a.b[2].c-d_e").is_ok());
    assert!(
        ctx.set("big", json!("x".repeat(MAX_DATA_STRING + 1)))
            .is_err()
    );
    let mut deep = json!(1);
    for _ in 0..MAX_DATA_DEPTH + 2 {
        deep = json!([deep]);
    }
    assert!(ctx.set("deep", deep).is_err());
    let version = ctx.version();
    assert!(ctx.remove("a.b[0]").unwrap());
    assert!(ctx.version() > version);
    assert_eq!(ctx.get("a.b").unwrap(), Some(&json!([])));
}

// ---- world-space panels ----------------------------------------------------

#[test]
fn world_panel_corners_follow_the_entity_transform() {
    let panel = UiWorldPanel::default(); // 2 x 1 world units, 100 px per unit
    assert_eq!(panel.pixel_size(), [200, 100]);
    let at = |c: [f32; 3], e: [f32; 3]| {
        for (a, b) in c.iter().zip(e) {
            near(*a, b);
        }
    };
    let matrix = Mat4::from_translation(Vec3::new(10.0, 0.0, 5.0));
    let corners = panel.corners(matrix);
    at(corners[0], [9.0, 0.5, 5.0]); // top-left
    at(corners[1], [11.0, 0.5, 5.0]);
    at(corners[2], [11.0, -0.5, 5.0]);
    at(corners[3], [9.0, -0.5, 5.0]);
    // rotated 90 degrees about Y and scaled 2x: local +x maps to world -z
    let matrix = Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0))
        * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
        * Mat4::from_scale(Vec3::splat(2.0));
    let corners = panel.corners(matrix);
    at(corners[0], [0.0, 2.0, 2.0]);
    at(corners[1], [0.0, 2.0, -2.0]);
    at(corners[3], [0.0, 0.0, 2.0]);
    // a pivot at the top-left corner puts pixel (0,0) at the entity origin
    let pivoted = UiWorldPanel {
        pivot: [0.0, 0.0],
        ..Default::default()
    };
    at(pivoted.corners(Mat4::IDENTITY)[0], [0.0, 0.0, 0.0]);
    at(pivoted.corners(Mat4::IDENTITY)[2], [2.0, -1.0, 0.0]);
}

#[test]
fn world_panel_picking_maps_rays_to_ui_pixels() {
    let panel = UiWorldPanel::default();
    let matrix = Mat4::from_translation(Vec3::new(10.0, 0.0, 5.0));
    let hit = panel
        .pick(matrix, Vec3::new(10.0, 0.0, 15.0), Vec3::NEG_Z)
        .unwrap();
    near(hit[0], 100.0);
    near(hit[1], 50.0);
    let hit = panel
        .pick(matrix, Vec3::new(9.5, 0.25, 15.0), Vec3::NEG_Z)
        .unwrap();
    near(hit[0], 50.0);
    near(hit[1], 25.0);
    assert!(
        panel
            .pick(matrix, Vec3::new(12.0, 0.0, 15.0), Vec3::NEG_Z)
            .is_none()
    );
    assert!(
        panel
            .pick(matrix, Vec3::new(10.0, 0.0, 15.0), Vec3::X)
            .is_none()
    );
    assert!(
        panel
            .pick(matrix, Vec3::new(10.0, 0.0, 0.0), Vec3::NEG_Z)
            .is_none(),
        "a ray pointing away from the panel misses"
    );
    // A perspective camera ray through the viewport centre looks down -Z.
    let camera = Mat4::from_translation(Vec3::new(10.0, 0.0, 15.0));
    let (origin, direction) = camera3d_ray(
        camera,
        &engine_project::rendering3d::Projection3D::default(),
        [320, 180],
        [160.0, 90.0],
    )
    .unwrap();
    let hit = panel.pick(matrix, origin, direction).unwrap();
    near(hit[0], 100.0);
    near(hit[1], 50.0);
    // Off-centre pixels fan out with the field of view (60 degrees vertical).
    let (_, direction) = camera3d_ray(
        camera,
        &engine_project::rendering3d::Projection3D::default(),
        [320, 180],
        [160.0, 0.0],
    )
    .unwrap();
    near(
        direction.y / -direction.z,
        (std::f32::consts::FRAC_PI_6).tan(),
    );
}

#[test]
fn billboard_panels_face_the_camera() {
    let rotated = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
    let panel = UiWorldPanel {
        billboard: UiBillboard::Full,
        ..Default::default()
    };
    let camera = Vec3::new(0.0, 0.0, 10.0);
    let placed = panel.placement(rotated, Some(camera));
    // The panel normal (+Z) points at the camera.
    let normal = placed.transform_vector3(Vec3::Z).normalize();
    near(normal.x, 0.0);
    near(normal.z, 1.0);
    for corner in panel.corners(placed) {
        near(corner[2], 0.0); // the quad lies in the z = 0 plane
    }
    // From the side, a yaw billboard turns about Y and stays upright.
    let yaw = UiWorldPanel {
        billboard: UiBillboard::Yaw,
        ..Default::default()
    };
    let placed = yaw.placement(Mat4::IDENTITY, Some(Vec3::new(10.0, 5.0, 0.0)));
    let normal = placed.transform_vector3(Vec3::Z).normalize();
    near(normal.x, 1.0);
    near(normal.y, 0.0);
    let up = placed.transform_vector3(Vec3::Y).normalize();
    near(up.y, 1.0);
    // Full billboarding tilts toward a raised camera.
    let placed = panel.placement(Mat4::IDENTITY, Some(Vec3::new(0.0, 10.0, 10.0)));
    near(
        placed.transform_vector3(Vec3::Z).normalize().y,
        std::f32::consts::FRAC_1_SQRT_2,
    );
    // Scale and position survive; no billboard or no camera leaves the matrix alone.
    let scaled =
        Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)) * Mat4::from_scale(Vec3::splat(3.0));
    near(
        panel
            .placement(scaled, Some(camera))
            .x_axis
            .truncate()
            .length(),
        3.0,
    );
    assert_eq!(panel.placement(scaled, None), scaled);
    assert_eq!(
        UiWorldPanel::default().placement(scaled, Some(camera)),
        scaled
    );
}

#[test]
fn geometry_clipping_cuts_quads_and_remaps_uvs() {
    let vertex = |x: f32, y: f32, u: f32, v: f32| Vertex {
        position: [x, y, 0.0],
        uv: [u, v],
        color: [1.0; 4],
    };
    // Same vertex order as the UI painter's textured quads: BL, BR, TR, TL.
    let mesh = Mesh {
        vertices: vec![
            vertex(0.0, 10.0, 0.0, 1.0),
            vertex(10.0, 10.0, 1.0, 1.0),
            vertex(10.0, 0.0, 1.0, 0.0),
            vertex(0.0, 0.0, 0.0, 0.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    };
    let clipped = clip_quads(&mesh, [5.0, 0.0, 10.0, 5.0]);
    assert_eq!(clipped.indices.len(), 6);
    let xs: Vec<f32> = clipped.vertices.iter().map(|v| v.position[0]).collect();
    let ys: Vec<f32> = clipped.vertices.iter().map(|v| v.position[1]).collect();
    assert_eq!(xs, [5.0, 10.0, 10.0, 5.0]);
    assert_eq!(ys, [5.0, 5.0, 0.0, 0.0]);
    let uv: Vec<[f32; 2]> = clipped.vertices.iter().map(|v| v.uv).collect();
    assert_eq!(uv, [[0.5, 0.5], [1.0, 0.5], [1.0, 0.0], [0.5, 0.0]]);
    // fully outside: dropped; fully inside: unchanged
    assert!(clip_quads(&mesh, [20.0, 20.0, 5.0, 5.0]).indices.is_empty());
    assert_eq!(
        clip_quads(&mesh, [-5.0, -5.0, 50.0, 50.0]).vertices,
        mesh.vertices
    );
}

fn panel_scene(extra: Value) -> Value {
    let mut toggle = node(
        "t",
        UiWidget::Toggle {
            text: String::new(),
            checked: false,
            action: "flip".into(),
        },
    );
    toggle.style.width = UiLength::Fill;
    toggle.style.height = UiLength::Fill;
    let panel_root = root(vec![toggle]);
    json!([entity(
        "panel",
        json!({
            "Transform":{"position":[0,0,0]},
            "custom":{
                "engine.UiRoot":{"version":1,"data":panel_root},
                "engine.UiWorldPanel":{"version":1,"data":extra}
            }
        })
    )])
}

#[test]
fn world_panel_renders_as_world_draws_and_receives_pointer_through_the_camera() {
    let (_dir, mut runtime) = runtime(panel_scene(json!({})));
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let frame = runtime.extract_frame().unwrap();
    assert!(!frame.draws.is_empty());
    for draw in &frame.draws {
        assert_eq!(draw.space, DrawSpace::World, "{}", draw.id);
        assert!(draw.id.starts_with("world:game-ui:"));
        assert!(draw.clip.is_none());
        for v in &draw.mesh.vertices {
            // inside the 2 x 1 quad centred on the origin (plus the z bias)
            assert!(v.position[0].abs() <= 1.0 + 1e-4 && v.position[1].abs() <= 0.5 + 1e-4);
            assert!(v.position[2].abs() <= 0.06);
        }
    }
    // A click away from the panel does nothing; a click on it toggles.
    let flipped = |runtime: &Runtime| {
        matches!(
            root_of(runtime, "panel").tree.nodes[0].widget,
            UiWidget::Toggle { checked: true, .. }
        )
    };
    runtime
        .tick(0.016, press("ui_click", [10.0, 10.0]))
        .unwrap();
    runtime.tick(0.016, at([10.0, 10.0])).unwrap();
    assert!(!flipped(&runtime));
    // default Camera2D: 320 x 180 logical pixels, centre (160, 90) is world (0, 0)
    runtime
        .tick(0.016, press("ui_click", [160.0, 90.0]))
        .unwrap();
    runtime.tick(0.016, at([160.0, 90.0])).unwrap();
    assert!(flipped(&runtime));
}

#[test]
fn non_interactive_world_panels_ignore_the_pointer() {
    let (_dir, mut runtime) = runtime(panel_scene(json!({"pointer":"none"})));
    runtime
        .tick(0.016, press("ui_click", [160.0, 90.0]))
        .unwrap();
    runtime.tick(0.016, at([160.0, 90.0])).unwrap();
    assert!(matches!(
        root_of(&runtime, "panel").tree.nodes[0].widget,
        UiWidget::Toggle { checked: false, .. }
    ));
}

// ---- descriptors and validation --------------------------------------------

fn validate(name: &str, data: Value) -> engine_project::Result<()> {
    ComponentCatalog::new(vec![])
        .unwrap()
        .validate(name, &CustomValue { version: 1, data })
}

#[test]
fn descriptors_accept_samples_and_reject_invalid_data() {
    let catalog = ComponentCatalog::new(vec![]).unwrap();
    for name in [
        "engine.UiRoot",
        "engine.UiTheme",
        "engine.UiData",
        "engine.UiWorldPanel",
    ] {
        assert!(catalog.0.contains_key(name), "{name}");
    }
    let good_theme = json!({
        "version":1,"name":"main","extends":"base",
        "tokens":{"colors":{"ink":[0,0,0,1]},"numbers":{"gap":8},"fonts":{"body":"font"}},
        "rules":[
            {"selector":"button.primary:hover, #ok > label","style":{"background":"$ink","padding":"$gap",
              "font":"$body","width":{"unit":"fill"},"layout":"row"},
             "transition":[{"property":"background","duration":0.2,"easing":{"bezier":[0.25,0.1,0.25,1]}},
                           {"property":"scale","duration":0.1,"delay":0.05,"easing":"ease_out"}],
             "animation":{"name":"pulse","duration":1.5,"iterations":"infinite","direction":"alternate"}}
        ],
        "keyframes":{"pulse":{"frames":[{"at":0,"opacity":1},{"at":1,"opacity":0.4,"scale":1.1}]}}
    });
    validate("engine.UiTheme", good_theme.clone()).unwrap();
    validate(
        "engine.UiData",
        json!({"version":1,"values":{"score":1,"inv":[{"n":"a"}]}}),
    )
    .unwrap();
    validate(
        "engine.UiWorldPanel",
        json!({"version":1,"size":[3,1.5],"pixelsPerUnit":64,
        "pivot":[0.5,1],"billboard":"yaw","pointer":"camera3d","keyboard":true}),
    )
    .unwrap();
    let mut bound = binding_root();
    bound.theme = Some("main".into());
    bound.reduced_motion = Some(true);
    validate("engine.UiRoot", serde_json::to_value(&bound).unwrap()).unwrap();
    let mut with_classes = root(vec![button("b")]);
    with_classes.tree.nodes[0].classes = vec!["primary".into()];
    with_classes.tree.nodes[0].style.opacity = 0.5;
    with_classes.tree.nodes[0].style.scale = 1.2;
    validate(
        "engine.UiRoot",
        serde_json::to_value(&with_classes).unwrap(),
    )
    .unwrap();

    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut v = good_theme.clone();
        f(&mut v);
        v
    };
    let bad_themes = [
        mutate(&|v| v["rules"][0]["selector"] = json!("bogus_kind")),
        mutate(&|v| v["rules"][0]["selector"] = json!("button:wobble")),
        mutate(&|v| v["rules"][0]["selector"] = json!("button >")),
        mutate(&|v| v["rules"][0]["selector"] = json!("button button button button button")),
        mutate(&|v| v["rules"][0]["selector"] = json!("")),
        mutate(&|v| v["extends"] = json!("main")),
        mutate(&|v| v["name"] = json!("bad name")),
        mutate(&|v| v["version"] = json!(2)),
        mutate(&|v| v["rules"][0]["transition"][0]["easing"] = json!({"bezier":[2,0,0.5,1]})),
        mutate(&|v| v["rules"][0]["transition"][0]["duration"] = json!(-1)),
        mutate(&|v| v["rules"][0]["animation"]["name"] = json!("missing")),
        mutate(&|v| v["rules"][0]["animation"]["duration"] = json!(0)),
        mutate(&|v| v["rules"][0]["animation"]["iterations"] = json!(0)),
        mutate(&|v| v["keyframes"]["pulse"]["frames"][1]["at"] = json!(0)),
        mutate(&|v| v["keyframes"]["pulse"]["frames"] = json!([])),
        mutate(&|v| v["rules"][0]["style"]["background"] = json!("$nope")),
        mutate(&|v| v["rules"][0]["style"]["background"] = json!([2, 0, 0, 1])),
        mutate(&|v| v["rules"][0]["style"]["bogusField"] = json!(1)),
        mutate(&|v| v["tokens"]["colors"]["ink"] = json!([0, 0, 0])),
        mutate(&|v| {
            v["rules"] = json!(
                (0..513)
                    .map(|_| json!({"selector":"button"}))
                    .collect::<Vec<_>>()
            )
        }),
    ];
    // `extends` themes resolve their tokens and keyframes at composition, so
    // those errors only surface for standalone themes; drop it for those cases.
    for (i, mut bad) in bad_themes.into_iter().enumerate() {
        if i != 5 {
            bad.as_object_mut().unwrap().remove("extends");
        }
        assert!(
            validate("engine.UiTheme", bad.clone()).is_err(),
            "accepted #{i}: {bad}"
        );
    }
    for bad in [
        json!({"version":1,"values":{"a.b":1}}),
        json!({"version":1,"values":{"a":"x".repeat(MAX_DATA_STRING + 1)}}),
        json!({"version":2,"values":{}}),
    ] {
        assert!(
            validate("engine.UiData", bad.clone()).is_err(),
            "accepted {bad}"
        );
    }
    for bad in [
        json!({"size":[0,1]}),
        json!({"pixelsPerUnit":0}),
        json!({"pixelsPerUnit":5000}),
        json!({"size":[100,100]}),
        json!({"pivot":[2,0]}),
        json!({"billboard":"sideways"}),
        json!({"pointer":"gamepad"}),
        json!({"extra":1}),
    ] {
        assert!(
            validate("engine.UiWorldPanel", bad.clone()).is_err(),
            "accepted {bad}"
        );
    }
    let binding_cases = |f: &dyn Fn(&mut UiRoot)| {
        let mut r = binding_root();
        f(&mut r);
        serde_json::to_value(&r).unwrap()
    };
    for (i, bad) in [
        binding_cases(&|r| r.bindings[0].node = "missing".into()),
        binding_cases(&|r| r.bindings[0].path = "bad path".into()),
        binding_cases(&|r| r.bindings[0].property = UiBindProperty::Value),
        binding_cases(&|r| r.bindings[0].mode = UiBindMode::TwoWay),
        binding_cases(&|r| r.bindings[1].format = Some("{x}".into())),
        binding_cases(&|r| r.bindings[0].format = Some("{unclosed".into())),
        binding_cases(&|r| r.bindings[0].format = Some("{a:.x}".into())),
        binding_cases(&|r| r.bindings[3].node = "hp".into()),
        binding_cases(&|r| r.bindings[4].node = "panel".into()),
        binding_cases(&|r| r.bindings.push(r.bindings[0].clone())),
        binding_cases(&|r| r.bindings = vec![r.bindings[0].clone(); MAX_UI_BINDINGS + 1]),
        binding_cases(&|r| r.bindings[0].item_path = Some("x".into())),
        binding_cases(&|r| r.theme = Some("bad name".into())),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            validate("engine.UiRoot", bad).is_err(),
            "accepted binding case #{i}"
        );
    }
    assert!(
        validate(
            "engine.UiRoot",
            json!({"bindings":[{"node":"a","property":"text"}]})
        )
        .is_err()
    );
}

#[test]
fn every_ui_style_field_can_be_overridden_by_a_style_delta() {
    let all = json!({
        "width":{"unit":"pixels","value":11},"height":{"unit":"percent","value":0.5},
        "minWidth":{"unit":"pixels","value":1},"minHeight":{"unit":"pixels","value":2},
        "maxWidth":{"unit":"pixels","value":300},"maxHeight":{"unit":"pixels","value":400},
        "aspectRatio":1.5,"offset":[3,4],"layout":"grid","position":"absolute",
        "inset":{"left":1,"top":2},"margin":{"left":1,"top":2,"right":3,"bottom":4},
        "padding":9,"paddingEdges":{"left":1,"top":2,"right":3,"bottom":4},"gap":7,
        "rowGap":5,"columnGap":6,"wrap":"wrap","justify":"center","alignItems":"end",
        "alignSelf":"center","alignContent":"space_between","justifyItems":"start",
        "justifySelf":"stretch","grow":2,"shrink":3,"basis":{"unit":"pixels","value":30},
        "gridColumns":[{"kind":"px","value":10}],"gridRows":[{"kind":"fr","value":1}],
        "gridAutoColumns":{"kind":"auto"},"gridAutoRows":{"kind":"min_content"},
        "gridAutoFlow":"column","gridColumn":{"start":1,"span":2},"gridRow":{"start":2},
        "clip":false,"background":[0.1,0.2,0.3,0.4],"foreground":[0.5,0.6,0.7,0.8],
        "accent":[0.9,0.8,0.7,0.6],"font":"serif","fontScale":1.25,"opacity":0.5,"scale":1.5
    });
    let expected: UiStyle = serde_json::from_value(all.clone()).unwrap();
    let delta: UiStyleDelta = serde_json::from_value(all).unwrap();
    let sheet = theme(json!({"version":1,"name":"all","rules":[
        {"selector":"button","style":serde_json::to_value(&delta).unwrap()}]}));
    let resolved = resolve(
        &root(vec![UiNode::button("b", "", "")]),
        &sheet,
        &no_states(),
    );
    assert_eq!(*style_of(&resolved, "b"), expected);
    // serialization round trip of a theme
    let round: UiTheme = serde_json::from_value(serde_json::to_value(&sheet).unwrap()).unwrap();
    assert_eq!(round, sheet);
}

// ---- serialized size -------------------------------------------------------

/// The ui-core measurement: 31 rows of 32 fully styled buttons.
fn inline_styled_grid() -> UiRoot {
    let mut nodes =
        vec![UiNode::container("root", UiLayout::Column).with_size(UiLength::Fill, UiLength::Fill)];
    for r in 0..31 {
        let row = format!("r{r}");
        nodes.push(
            UiNode::container(&row, UiLayout::Row)
                .with_size(UiLength::Fill, UiLength::Pixels(20.0))
                .child_of("root"),
        );
        for c in 0..32 {
            nodes.push(
                UiNode::button(format!("b{r}_{c}"), "AA", "")
                    .with_size(UiLength::Fill, UiLength::Fill)
                    .with_style(|s| s.font = "font".into())
                    .child_of(&row),
            );
        }
    }
    UiRoot {
        tree: UiTree { nodes },
        ..Default::default()
    }
}

/// The same grid with the styles moved into a theme: rows by class, buttons
/// by `.row > button`.
fn classed_grid() -> UiRoot {
    let mut grid = inline_styled_grid();
    for node in &mut grid.tree.nodes {
        match node.widget {
            UiWidget::Container if node.id.starts_with('r') => {
                node.style = UiStyle::default();
                node.classes = vec!["row".into()];
            }
            UiWidget::Container => {
                node.style = UiStyle::default();
                node.classes = vec!["col".into()];
            }
            _ => node.style = UiStyle::default(),
        }
    }
    grid
}

/// A realistic large screen: 127 shop cards of 8 nodes each (six layout
/// containers, a name label and a buy button) plus the list root.
fn card_screen() -> UiRoot {
    let mut nodes = vec![node("root", UiWidget::Container)];
    nodes[0].classes = vec!["list".into()];
    nodes[0].style = UiStyle::default();
    for i in 0..127 {
        let card = format!("k{i}");
        let (head, body, foot) = (format!("h{i}"), format!("d{i}"), format!("f{i}"));
        let mut add = |n: UiNode, class: &str, parent: &str| {
            let mut n = child(n, parent);
            n.style = UiStyle::default();
            n.classes = vec![class.into()];
            nodes.push(n);
        };
        add(UiNode::new(&card, UiWidget::Container), "card", "root");
        add(UiNode::new(&head, UiWidget::Container), "head", &card);
        add(
            UiNode::new(format!("i{i}"), UiWidget::Container),
            "icon",
            &head,
        );
        add(
            UiNode::label(format!("t{i}"), format!("Item {i}")),
            "name",
            &head,
        );
        add(UiNode::new(&body, UiWidget::Container), "body", &card);
        add(UiNode::new(&foot, UiWidget::Container), "foot", &body);
        add(
            UiNode::new(format!("g{i}"), UiWidget::Container),
            "badge",
            &foot,
        );
        add(UiNode::button(format!("b{i}"), "Buy", "buy"), "buy", &foot);
    }
    UiRoot {
        tree: UiTree { nodes },
        theme: Some("shop".into()),
        ..Default::default()
    }
}

fn shop_theme() -> UiTheme {
    theme(json!({
    "version":1,"name":"shop",
    "tokens":{"colors":{"panel":[0.12,0.14,0.2,1],"accent":[0.3,0.6,1,1]},"numbers":{"gap":6}},
    "rules":[
        {"selector":".list","style":{"layout":"column","gap":"$gap","width":{"unit":"fill"},"height":{"unit":"fill"}}},
        {"selector":".card","style":{"background":"$panel","layout":"column","height":{"unit":"pixels","value":48},"width":{"unit":"fill"},"gap":"$gap"}},
        {"selector":".card > .head","style":{"layout":"row","height":{"unit":"pixels","value":24}}},
        {"selector":".head > .icon","style":{"background":"$accent","width":{"unit":"pixels","value":24}}},
        {"selector":".name","style":{"foreground":[1,1,1,1]}},
        {"selector":".foot","style":{"layout":"row"}},
        {"selector":".buy","style":{"background":"$accent"},
         "transition":[{"property":"background","duration":0.15}]},
        {"selector":".buy:hover","style":{"background":[0.4,0.7,1,1]}}
    ]}))
}

#[test]
fn stylesheet_based_trees_fit_the_64_kib_component_limit() {
    let limit = 65_536;
    let size = |r: &UiRoot| serde_json::to_vec(r).unwrap().len();
    let inline = inline_styled_grid();
    let classed = classed_grid();
    let cards = card_screen();
    let (inline_bytes, classed_bytes, card_bytes) = (size(&inline), size(&classed), size(&cards));
    // The same card screen with the theme's look written inline on every node.
    let inline_cards = {
        let mut r = resolve(&cards, &shop_theme(), &no_states());
        r.theme = None;
        r
    };
    let inline_card_bytes = size(&inline_cards);
    println!(
        "ui style sizes: inline 1024-button grid {inline_bytes} B; same grid with classes and a theme {classed_bytes} B; 1024-node card screen with classes {card_bytes} B, with the same look inline {inline_card_bytes} B ({} nodes); limit {limit} B",
        cards.tree.nodes.len()
    );
    assert!(
        inline_card_bytes > limit,
        "inline card screen is {inline_card_bytes} B"
    );
    assert!(
        inline_bytes > limit,
        "the baseline does not fit: {inline_bytes}"
    );
    assert!(
        classed_bytes * 100 / inline_bytes < 50,
        "classes alone halve the size"
    );
    assert_eq!(cards.tree.nodes.len(), 1017);
    assert!(card_bytes < limit, "card screen is {card_bytes} B");
    // The authored component passes the catalog bound and validates as a tree.
    validate("engine.UiRoot", serde_json::to_value(&cards).unwrap()).unwrap();
    // And the theme that restores the look is a small component of its own.
    let shop = shop_theme();
    validate("engine.UiTheme", serde_json::to_value(&shop).unwrap()).unwrap();
    // The themed tree lays out and draws as a whole.
    let resolved = resolve(&cards, &shop, &no_states());
    assert_eq!(resolved.tree.nodes.len(), cards.tree.nodes.len());
    let boxes = resolved.tree.layout([1280, 720]).unwrap();
    assert_eq!(boxes.len(), cards.tree.nodes.len());
    let card = style_of(&resolved, "k5");
    assert_eq!(card.height, UiLength::Pixels(48.0));
}
