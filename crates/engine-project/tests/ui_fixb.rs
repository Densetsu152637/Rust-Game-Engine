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
        UiNode::rich_label("bad", "[b]open [size=9999]x"),
        UiNode::rich_label("angle", "<b>Bold</b> text"),
    ]);
    let (_dir, mut runtime) = runtime(json!([ui_entity("hud", &root)]));
    enable_access(&mut runtime);
    runtime.tick(0.016, ActionFrame::default()).unwrap();
    let name = |id: &str| tree(&runtime).find("hud", id).unwrap().name.clone();
    assert_eq!(name("bold").as_deref(), Some("Bold and red"));
    // Escapes resolve and an unknown `[note]` stays text, as it does when drawn.
    assert_eq!(name("escaped").as_deref(), Some("Press [Enter] [note]"));
    // Unparseable markup never leaks raw tags into the name.
    assert_eq!(name("bad").as_deref(), Some("open x"));
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
