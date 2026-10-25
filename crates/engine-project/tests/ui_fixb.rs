#![recursion_limit = "512"]
//! Regression tests for the UI critic's fixb findings: accessible names of
//! rich labels and virtual-list rows, and the theme cascade budget.
use bevy_ecs::prelude::World;
use engine_project::{ActionFrame, Project, Runtime, ui_tree::*};
use serde_json::{Value, json};

fn root(nodes: Vec<UiNode>) -> UiRoot {
    UiRoot {
        tree: UiTree { nodes },
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
            &json!({"schema":1,"name":"fixb","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    (dir, runtime)
}

fn ui_entity(id: &str, root: &UiRoot) -> Value {
    json!({"id":id,"name":id,"components":{"custom":{"engine.UiRoot":{"version":1,"data":root}}}})
}

fn world(runtime: &Runtime) -> &World {
    runtime.app().world()
}

fn enable_access(runtime: &mut Runtime) {
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<GameUi>()
        .set_accessibility(true);
}

fn tree(runtime: &Runtime) -> &AccessTree {
    world(runtime)
        .resource::<GameUi>()
        .accessibility_tree()
        .expect("accessibility tree")
}

// ---- M3: rich label names ---------------------------------------------------

#[test]
fn rich_label_names_drop_bracket_markup_and_resolve_escapes() {
    let root = root(vec![
        UiNode::rich_label("bold", "[b]Bold[/b] and [color=#ff0000]red[/color]"),
        UiNode::rich_label("escaped", r"Press \[Enter\] [note]"),
        UiNode::rich_label("angle", "<b>Bold</b> text"),
    ]);
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &root)]));
    enable_access(&mut runtime);
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let name = |id: &str| tree(&runtime).find("hud", id).unwrap().name.clone();
    assert_eq!(name("bold").as_deref(), Some("Bold and red"));
    // Escapes resolve and an unknown `[note]` stays text, as it does when drawn.
    assert_eq!(name("escaped").as_deref(), Some("Press [Enter] [note]"));
    // Malformed authored markup such as "[b]open [size=9999]x" is rejected by
    // UiRoot validation (ui_hardening tests); the lenient name fallback for it
    // is covered by the ui_tree::text unit tests.
    assert_eq!(name("angle").as_deref(), Some("Bold text"));
}

// ---- M4: virtual-list row names ----------------------------------------------

fn list_root() -> UiRoot {
    let mut root = root(vec![
        UiNode::virtual_list("inv", 0, 20.0, "row")
            .with_size(UiLength::Pixels(200.0), UiLength::Pixels(100.0)),
        UiNode::container("row", UiLayout::Row).child_of("inv"),
        UiNode::label("rowtext", "template").child_of("row"),
    ]);
    root.bindings = vec![
        UiBinding::one_way("inv", UiBindProperty::ItemCount, "items"),
        UiBinding::one_way("rowtext", UiBindProperty::ItemText, "items")
            .with_item_path("name")
            .with_format("{#}:{}"),
    ];
    root
}

#[test]
fn virtual_list_rows_in_the_access_tree_carry_their_bound_item_text() {
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &list_root())]));
    enable_access(&mut runtime);
    runtime
        .app_mut()
        .world_mut()
        .resource_mut::<UiDataContext>()
        .set(
            "items",
            json!([{"name":"Axe"},{"name":"Bow"},{"name":"Cap"}]),
        )
        .unwrap();
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let mut names: Vec<String> = (0..3)
        .map(|item| {
            let id = AccessId::node("hud", "rowtext").with_item(item);
            tree(&runtime)
                .get(&id)
                .unwrap_or_else(|| panic!("no row {item}"))
                .name
                .clone()
                .unwrap_or_default()
        })
        .collect();
    names.dedup();
    assert_eq!(names.len(), 3, "rows must have distinct names: {names:?}");
    assert!(names[1].contains("Bow"), "{names:?}");
}

// ---- L1: cascade budget ------------------------------------------------------

fn worst_theme(rules: usize, selector: &str, lists: usize) -> UiTheme {
    let list = vec![selector; lists].join(", ");
    serde_json::from_value(json!({
        "version":1,"name":"worst",
        "rules": (0..rules).map(|_| json!({"selector": list, "style":{"opacity":0.5}})).collect::<Vec<_>>()
    }))
    .unwrap()
}

/// `chains` paths of `depth` containers each under one root container.
fn deep_root(chains: usize, depth: usize) -> UiRoot {
    let mut nodes = vec![UiNode::container("r", UiLayout::Column)];
    for c in 0..chains {
        let mut parent = "r".to_owned();
        for d in 0..depth {
            let id = format!("n{c}_{d}");
            nodes.push(UiNode::container(id.clone(), UiLayout::Column).child_of(parent));
            parent = id;
        }
    }
    nodes.truncate(1024);
    root(nodes)
}

fn compose(rules: usize, selector: &str, lists: usize) -> Result<UiComposedTheme, String> {
    let mut set = UiThemeSet::default();
    set.insert(worst_theme(rules, selector, lists))
        .and_then(|_| set.compose("worst"))
        .map_err(|e| e.diagnostics[0].code.clone())
}

#[test]
fn a_theme_that_would_stall_the_cascade_is_rejected_when_composed() {
    // 2048 compiled rules of four chained descendant compounds: ~7.5 s per cascade (debug)
    // on a 1009-node tree before the budget existed.
    assert_eq!(
        compose(256, "* * * *", 8).err().as_deref(),
        Some("ui.style.cascade_budget")
    );
    // The same shape within the budget composes and cascades.
    let theme = compose(200, "* * * *", 1).expect("200 rules fit the budget");
    let started = std::time::Instant::now();
    let out = UiStyleRuntime::default()
        .resolve("hud", &deep_root(16, 63), &theme, &Default::default())
        .unwrap();
    println!("worst allowed cascade: {:?}", started.elapsed());
    assert_eq!(out.tree.nodes.len(), 1009);
    // Nodes under at least three ancestors match `* * * *`.
    let deep = out.tree.find("n0_5").unwrap();
    assert_eq!(out.tree.nodes[deep].style.opacity, 0.5);
}

#[test]
fn ordinary_themes_stay_far_inside_the_budget() {
    // The most rules a theme may compile, as plain selectors.
    assert!(compose(MAX_COMPILED_RULES / 8, "button", 8).is_ok());
    // Hundreds of two-level descendant rules.
    assert!(compose(400, ".menu .item", 1).is_ok());
    assert!(compose(250, ".menu .item .row", 1).is_ok());
    // The budget is on the sum: too many descendant rules are refused...
    assert_eq!(
        compose(400, ".menu .item .row", 1).err().as_deref(),
        Some("ui.style.cascade_budget")
    );
    // ...and the limit on compiled rules still reports first when it is the cause.
    assert!(compose(MAX_COMPILED_RULES / 8 + 1, "button", 8).is_err());
}

#[test]
fn the_budget_covers_the_whole_extends_chain() {
    let theme = |name: &str, extends: Option<&str>, rules: usize| -> UiTheme {
        let mut theme = worst_theme(rules, ".a .b .c", 1);
        theme.name = name.into();
        theme.extends = extends.map(str::to_owned);
        theme
    };
    let mut set = UiThemeSet::default();
    // 200 rules of cost 8 + 2 * 64 = 136 each: 27 200 alone, 54 400 together.
    set.insert(theme("base", None, 200)).unwrap();
    set.insert(theme("derived", Some("base"), 200)).unwrap();
    assert!(set.compose("base").is_ok());
    assert_eq!(
        set.compose("derived")
            .err()
            .map(|e| e.diagnostics[0].code.clone())
            .as_deref(),
        Some("ui.style.cascade_budget")
    );
}

#[test]
fn an_authored_theme_over_budget_is_rejected_by_validation() {
    let theme = worst_theme(256, "* * * *", 8);
    let error = UiThemeSet::default().insert(theme).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "ui.style.cascade_budget");
}

// ---- L4: text render status per path -----------------------------------------

#[test]
fn screen_only_sdf_text_is_not_reported_as_a_fallback_for_a_missing_3d_camera() {
    const FONT: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ui.ttf"), FONT).unwrap();
    std::fs::write(
        dir.path().join("fonts.json"),
        json!({"schema":1,"defaultFamily":"ui","families":{"ui":{"size":18,"atlas":"sdf"}}})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("project.json"),
        json!({"schema":1,"name":"status","startup_scene":"scene.json","assets":[
            {"id":"ui","kind":"font_face","path":"ui.ttf"},
            {"id":"fonts","kind":"font_config","path":"fonts.json"}]})
        .to_string(),
    )
    .unwrap();
    let hud = json!({"version":1,"tree":{"nodes":[
        {"id":"label","widget":{"kind":"label","text":"Score 120"},
         "style":{"font":"ui","width":{"unit":"pixels","value":200},"height":{"unit":"pixels","value":30}}}]}});
    std::fs::write(
        dir.path().join("scene.json"),
        json!({"schema":1,"scene_id":"main","entities":[
            {"id":"hud","name":"HUD","components":{"custom":{"engine.UiRoot":{"version":1,"data":hud}}}}]})
        .to_string(),
    )
    .unwrap();
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.start().unwrap();
    let queue = world(&runtime).resource::<UiGpuQueue>().clone();
    for feature in UiGpuFeature::ALL {
        queue.mark_registered(feature);
        queue.set_ready(feature, true);
    }
    runtime.extract_frame().unwrap();
    let status = runtime.text_engine().unwrap().render_status();
    // No Camera3D: the world path cannot draw, but nothing asks it to.
    assert_eq!(status.world.state, UiFeatureState::Fallback);
    assert_eq!(status.screen.state, UiFeatureState::Gpu);
    assert_eq!(status.state, TextRenderState::Sdf, "{status:?}");
    assert!(status.fallback_reason.is_none(), "{status:?}");
}

// ---- L6: shaped-text cache eviction --------------------------------------------

#[test]
fn the_shaped_text_cache_keeps_recent_layouts_past_its_capacity() {
    use engine_project::content::{FontAtlasMode, FontFamilySettings};
    let mut engine = EngineTextBackend::new(engine_text::AtlasConfig::default()).unwrap();
    engine
        .register_font(
            epaint_default_fonts::UBUNTU_LIGHT.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(16.0),
                atlas: FontAtlasMode::Coverage,
            },
        )
        .unwrap();
    let texts: Vec<String> = (0..700).map(|n| format!("line {n}")).collect();
    for text in &texts {
        engine
            .measure(&UiTextRequest {
                text,
                kind: UiTextKind::Plain,
                font: "ui",
                scale: 1.0,
                color: [1.0; 4],
                max_width: None,
            })
            .unwrap();
    }
    // Wholesale clearing left ~190 entries after the 513th miss; eviction by recency keeps
    // the most recent ~three quarters of a full cache.
    let cached = engine.stats().cached_layouts;
    assert!((384..=512).contains(&cached), "cached {cached}");
}

// ---- L7: caption import ------------------------------------------------------

fn cue_text(source: &str) -> String {
    let srt = format!("1\n00:00:01,000 --> 00:00:02,000\n{source}\n");
    engine_project::captions::import::import_captions(&srt)
        .unwrap_or_else(|e| panic!("{source:?}: {e}"))
        .cues[0]
        .text
        .clone()
}

#[test]
fn caption_import_repairs_unbalanced_style_tags() {
    // A single unclosed `<i>` used to fail the whole import.
    assert_eq!(cue_text("<i>whispers"), "[i]whispers[/i]");
    assert_eq!(cue_text("stray</i> close"), "stray close");
    assert_eq!(cue_text("<b><i>x</b>y</i>"), "[b][i]x[/i][/b]y");
    assert_eq!(cue_text("<b>a</b></b>"), "[b]a[/b]");
    assert_eq!(cue_text("<b>a</u> b"), "[b]a b[/b]");
    // Well-formed input is unchanged, escapes still apply.
    assert_eq!(cue_text("<b>[x]</b>"), "[b]\\[x\\][/b]");
    // Pathological nesting is bounded rather than rejected.
    let deep = "<b>".repeat(100) + "x";
    let text = cue_text(&deep);
    assert!(text.starts_with("[b]") && text.contains('x'));
    assert_eq!(text.matches("[b]").count(), text.matches("[/b]").count());
}

#[test]
fn caption_import_reads_voice_annotations_past_class_names() {
    let cue = |start: u32, text: &str| {
        format!(
            "00:00:{start:02}.000 --> 00:00:{:02}.000
{text}

",
            start + 1
        )
    };
    let vtt = [
        "WEBVTT

"
        .to_owned(),
        cue(1, "<v.loud Bob>Hey</v>"),
        cue(3, "<v.a.b Mary Ann>Hi</v>"),
        cue(5, "<v Bob>Again</v>"),
        cue(7, "<v.loud>no speaker</v>"),
    ]
    .concat();
    let track = engine_project::captions::import::import_captions(&vtt).unwrap();
    let names: Vec<&str> = track.speakers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Bob", "Mary Ann"], "{names:?}");
    assert_eq!(track.cues[0].speaker, track.cues[2].speaker);
    assert_eq!(track.cues[3].speaker, None);
    assert_eq!(track.cues[3].text, "no speaker");
}
