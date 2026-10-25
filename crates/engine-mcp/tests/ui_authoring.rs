//! UI authoring tools: `ui_inspect` and `ui_preview` (read) and `ui_edit`
//! (write) over an entity's `engine.UiRoot`.
mod common;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

fn px(v: f64) -> Value {
    json!({"unit": "pixels", "value": v})
}

fn add_fonts(root: &Path) {
    fs::write(
        root.join("assets/ui.ttf"),
        epaint_default_fonts::UBUNTU_LIGHT,
    )
    .unwrap();
    fs::write(
        root.join("assets/fonts.json"),
        json!({"schema":1,"families":{"ui":{"size":20}},"fallback":{"default":["ui"]}}).to_string(),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
    let assets = manifest["assets"].as_array_mut().unwrap();
    assets.push(json!({"id":"ui","kind":"font_face","path":"assets/ui.ttf"}));
    assets.push(json!({"id":"fonts","kind":"font_config","path":"assets/fonts.json"}));
    fs::write(root.join("project.json"), manifest.to_string()).unwrap();
}

fn menu(title: &str) -> Value {
    json!({"version":1,"tree":{"nodes":[
        {"id":"panel","style":{"width":px(200.0),"height":px(80.0),"layout":"column",
            "padding":4,"gap":4,"background":[0.2,0.2,0.3,1.0]}},
        {"id":"title","parent":"panel","widget":{"kind":"label","text":title},
            "style":{"width":px(192.0),"height":px(24.0),"padding":0,"font":"ui",
                "background":[0,0,0,0]}},
        {"id":"go","parent":"panel","widget":{"kind":"button","text":"Go","action":"go"},
            "classes":["primary"],
            "style":{"width":px(80.0),"height":px(28.0),"font":"ui"}},
    ]}})
}

/// Installs the `engine.UiRoot` on the `player` entity.
fn set_ui(root: &Path, data: Value) -> Value {
    let set = write(
        root,
        "component_set",
        json!({"scene":START,"expected_revision":revision(root, START),"entity":"player",
            "component":"engine.UiRoot","value":{"version":1,"data":data}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    set
}

fn inspect(root: &Path, extra: Value) -> Value {
    let mut args = json!({"scene":START,"entity":"player"});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    data(root, "ui_inspect", args)
}

fn preview(root: &Path, extra: Value) -> Value {
    let mut args = json!({"scene":START,"entity":"player"});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    result(root, "ui_preview", args)
}

fn png_size(result: &Value) -> (u32, u32, Vec<u8>) {
    let image = &result["content"][1];
    assert_eq!(image["type"], "image", "{result}");
    assert_eq!(image["mimeType"], "image/png");
    let bytes = STANDARD.decode(image["data"].as_str().unwrap()).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&bytes[12..16], b"IHDR");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (word(16), word(20), bytes)
}

fn node<'a>(inspection: &'a Value, id: &str) -> &'a Value {
    inspection["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
        .unwrap_or_else(|| panic!("no node {id} in {inspection}"))
}

// ---- ui_inspect ----

#[test]
fn ui_inspect_reports_layout_hit_tests_and_hashes() {
    let dir = fixture();
    add_fonts(dir.path());
    set_ui(dir.path(), menu("Hello"));
    let out = inspect(dir.path(), json!({"viewport":[320,240],"point":[10,40]}));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["entity"], "player");
    assert_eq!(out["viewport"], json!([320, 240]));
    assert_eq!(out["node_count"], 3);
    assert_eq!(out["text_backend"], "shaped");
    assert_eq!(out["font"], "ui", "the only font is the default");
    assert_eq!(out["ui_hash"].as_str().unwrap().len(), 64);
    assert_eq!(out["revision"], revision(dir.path(), START));
    assert_eq!(out["truncated"], false);
    assert!(out["unplaced"].as_array().unwrap().is_empty());

    // Paint order: parent before children.
    let order: Vec<&str> = out["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["panel", "title", "go"]);
    assert_eq!(node(&out, "panel")["rect"], json!([0.0, 0.0, 200.0, 80.0]));
    assert_eq!(node(&out, "panel")["children"], json!(["title", "go"]));
    assert_eq!(node(&out, "title")["rect"], json!([4.0, 4.0, 192.0, 24.0]));
    assert_eq!(node(&out, "go")["rect"], json!([4.0, 32.0, 80.0, 28.0]));
    assert_eq!(node(&out, "go")["kind"], "button");
    assert_eq!(node(&out, "go")["parent"], "panel");
    assert_eq!(node(&out, "go")["clip"], json!([0.0, 0.0, 200.0, 80.0]));
    assert_eq!(node(&out, "panel")["clip"], json!([0.0, 0.0, 320.0, 240.0]));

    assert_eq!(out["hit"]["id"], "go", "{out}");
    assert_eq!(out["hit"]["interactive"], "go");
    assert_eq!(out["hit"]["stack"], json!(["go", "panel"]));
    let miss = inspect(dir.path(), json!({"point":[900,900]}));
    assert_eq!(miss["hit"]["id"], Value::Null);

    // Equal roots have equal hashes; an edit changes it.
    let again = inspect(dir.path(), json!({}));
    assert_eq!(again["ui_hash"], out["ui_hash"]);
    set_ui(dir.path(), menu("Other"));
    assert_ne!(inspect(dir.path(), json!({}))["ui_hash"], out["ui_hash"]);

    // Paging.
    let page = inspect(dir.path(), json!({"offset":1,"limit":1}));
    assert_eq!(page["returned"], 1);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 2);
    assert_eq!(page["nodes"][0]["id"], "title");
}

#[test]
fn ui_inspect_reports_issues_and_validates_arguments() {
    let dir = fixture();
    add_fonts(dir.path());
    set_ui(
        dir.path(),
        json!({"version":1,"tree":{"nodes":[
            {"id":"panel","style":{"width":px(100.0),"height":px(40.0),"padding":0}},
            {"id":"wide","parent":"panel","widget":{"kind":"button","text":"x"},
                "style":{"width":px(160.0),"height":px(20.0),"font":"ui"}},
            {"id":"flat","parent":"panel","style":{"width":px(0.0),"height":px(8.0)}},
        ]}}),
    );
    let out = inspect(dir.path(), json!({}));
    let codes: Vec<(&str, &str)> = out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["code"].as_str().unwrap(), i["node"].as_str().unwrap()))
        .collect();
    assert!(codes.contains(&("ui.issue.overflow", "wide")), "{out}");
    assert!(codes.contains(&("ui.issue.zero_size", "flat")), "{out}");

    for (extra, what) in [
        (json!({"viewport":[0,10]}), "viewport"),
        (json!({"viewport":[10]}), "viewport"),
        (json!({"point":[1]}), "point"),
        (json!({"states":{"ghost":["hovered"]}}), "states"),
        (json!({"states":{"go":["sparkly"]}}), "states"),
    ] {
        let mut args = json!({"scene":START,"entity":"player"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let failed = data(dir.path(), "ui_inspect", args);
        assert_eq!(error_code(&failed), "invalid_argument", "{what}: {failed}");
    }
    let missing = data(
        dir.path(),
        "ui_inspect",
        json!({"scene":START,"entity":"camera"}),
    );
    assert_eq!(error_code(&missing), "ui_root_missing");
    let ghost = data(
        dir.path(),
        "ui_inspect",
        json!({"scene":START,"entity":"ghost"}),
    );
    assert_eq!(error_code(&ghost), "entity_not_found");
    let unknown_theme = data(
        dir.path(),
        "ui_inspect",
        json!({"scene":START,"entity":"player","theme":"nope"}),
    );
    assert_eq!(error_code(&unknown_theme), "ui.theme.unknown");
}

#[test]
fn ui_inspect_works_without_fonts_and_says_so() {
    let dir = fixture();
    set_ui(
        dir.path(),
        json!({"version":1,"tree":{"nodes":[
            {"id":"go","widget":{"kind":"button","text":"Go"},
                "style":{"width":px(80.0),"height":px(28.0)}}]}}),
    );
    let out = inspect(dir.path(), json!({}));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["text_backend"], "none");
    assert!(out["warnings"][0].as_str().unwrap().contains("no fonts"));
    assert_eq!(node(&out, "go")["rect"], json!([0.0, 0.0, 80.0, 28.0]));
    // A preview of text without any font fails with advice instead of guessing.
    let failed = result(
        dir.path(),
        "ui_preview",
        json!({"scene":START,"entity":"player"}),
    );
    let failed = failed["structuredContent"].clone();
    assert_eq!(failed["ok"], false);
    assert!(
        failed["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("render.font"),
        "{failed}"
    );
    assert!(failed["error"]["hint"].as_str().unwrap().contains("font"));
}

// ---- ui_preview ----

#[test]
fn ui_preview_returns_png_image_content() {
    let dir = fixture();
    add_fonts(dir.path());
    set_ui(dir.path(), menu("Hello"));
    let out = preview(dir.path(), json!({"viewport":[200,80]}));
    assert_eq!(out["isError"], false, "{out}");
    let (width, height, bytes) = png_size(&out);
    assert_eq!((width, height), (200, 80));
    let structured = &out["structuredContent"];
    assert_eq!(structured["width"], 200);
    assert_eq!(structured["height"], 80);
    assert_eq!(structured["png_bytes"], bytes.len());
    assert!(structured["draws"].as_u64().unwrap() >= 3);
    let digest = structured["digest"].as_str().unwrap().to_owned();

    // Deterministic, and sensitive to content: no title text, no glyph pixels.
    let again = preview(dir.path(), json!({"viewport":[200,80]}));
    assert_eq!(again["structuredContent"]["digest"], digest);
    set_ui(dir.path(), menu(""));
    let blank = preview(dir.path(), json!({"viewport":[200,80]}));
    assert_ne!(blank["structuredContent"]["digest"], digest);

    // Scale and background.
    let big = preview(
        dir.path(),
        json!({"viewport":[200,80],"scale":2,"background":[1,1,1,1],"sampling":"bilinear"}),
    );
    let (width, height, _) = png_size(&big);
    assert_eq!((width, height), (400, 160));
}

#[test]
fn ui_preview_applies_themes_states_and_bounds_the_image() {
    let dir = fixture();
    add_fonts(dir.path());
    set_ui(dir.path(), menu("Hello"));
    let theme = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":revision(dir.path(), START),"entity":"camera",
            "component":"engine.UiTheme","value":{"version":1,"data":{"version":1,"name":"night",
                "tokens":{},"rules":[
                    {"selector":".primary","style":{"background":[1,0,0,1]}},
                    {"selector":".primary:hover","style":{"background":[0,1,0,1]}}]}}}),
    );
    assert_eq!(theme["ok"], true, "{theme}");
    let digest = |extra: Value| {
        let mut args = json!({"viewport":[200,80]});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let out = preview(dir.path(), args);
        assert_eq!(out["isError"], false, "{out}");
        out["structuredContent"]["digest"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let plain = digest(json!({}));
    let themed = digest(json!({"theme":"night"}));
    let hovered = digest(json!({"theme":"night","states":{"go":["hovered"]}}));
    assert_ne!(plain, themed);
    assert_ne!(themed, hovered);
    // Unthemed states are inert: no rule matches them.
    assert_eq!(plain, digest(json!({"states":{"go":["hovered"]}})));
    assert_eq!(plain, digest(json!({"theme":""})));
    let out = preview(dir.path(), json!({"theme":"night"}));
    assert_eq!(out["structuredContent"]["theme"], "night");

    for (extra, code) in [
        (json!({"viewport":[4000,4000]}), "ui.preview.size"),
        (json!({"viewport":[1000,1000],"scale":4}), "ui.preview.size"),
        (json!({"scale":0}), "invalid_argument"),
        (json!({"background":[1,1,1]}), "invalid_argument"),
        (json!({"sampling":"smooth"}), "invalid_argument"),
        (json!({"locale":"fr"}), "ui_locale_unavailable"),
    ] {
        let mut args = json!({"scene":START,"entity":"player"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let failed = data(dir.path(), "ui_preview", args);
        assert_eq!(error_code(&failed), code, "{extra}: {failed}");
    }
}

// ---- ui_edit ----

fn edit(root: &Path, revision: &str, extra: Value) -> Value {
    let mut args = json!({"scene":START,"expected_revision":revision,"entity":"player"});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    write(root, "ui_edit", args)
}

fn stored(root: &Path) -> Value {
    data(root, "entity_get", json!({"scene":START,"id":"player"}))["entity"]["components"]["custom"]
        ["engine.UiRoot"]["data"]
        .clone()
}

#[test]
fn ui_edit_applies_a_batch_and_reports_it() {
    let dir = fixture();
    add_fonts(dir.path());
    set_ui(dir.path(), menu("Hello"));
    let before = inspect(dir.path(), json!({}));
    let revision_before = revision(dir.path(), START);
    let out = edit(
        dir.path(),
        &revision_before,
        json!({"expected_hash":before["ui_hash"],"operations":[
            {"op":"add_node","id":"quit","widget":{"kind":"button","text":"Quit","action":"quit"},
                "parent":"panel","node":{"style":{"width":px(80.0),"height":px(28.0),"font":"ui"}}},
            {"op":"set_style","id":"panel","patch":{"height":px(120.0)}},
            {"op":"rename_node","id":"go","new_id":"play"},
            {"op":"set_classes","id":"play","classes":["primary","wide"]},
        ]}),
    );
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["saved"], true);
    assert_eq!(out["entity"], "player");
    assert_eq!(out["previous_revision"], revision_before);
    assert_ne!(out["revision"], revision_before);
    assert_eq!(out["revision"], revision(dir.path(), START));
    assert_eq!(out["previous_ui_hash"], before["ui_hash"]);
    assert_eq!(out["node_count"], 4);
    assert_eq!(out["reports"].as_array().unwrap().len(), 4);
    assert_eq!(out["reports"][2]["id_map"], json!({"go":"play"}));
    assert_eq!(out["changed_ids"], json!(["go", "panel", "play", "quit"]));
    assert_eq!(out["diff"]["added"], json!(["play", "quit"]));
    assert_eq!(out["diff"]["removed"], json!(["go"]));

    let nodes = stored(dir.path())["tree"]["nodes"].clone();
    let ids: Vec<&str> = nodes
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["panel", "title", "play", "quit"]);
    assert_eq!(nodes[2]["classes"], json!(["primary", "wide"]));
    assert_eq!(nodes[0]["style"]["height"], px(120.0));

    // The edit is visible to ui_inspect and its hash matches the result.
    let after = inspect(dir.path(), json!({}));
    assert_eq!(after["ui_hash"], out["ui_hash"]);
    assert_eq!(node(&after, "quit")["rect"], json!([4.0, 64.0, 80.0, 28.0]));
    assert_eq!(
        node(&after, "panel")["rect"],
        json!([0.0, 0.0, 200.0, 120.0])
    );
}

#[test]
fn ui_edit_rejects_stale_input_and_invalid_operations_without_writing() {
    let dir = fixture();
    set_ui(dir.path(), menu("Hello"));
    let file = || fs::read(dir.path().join(START)).unwrap();
    let original = file();
    let current = revision(dir.path(), START);
    let hash = inspect(dir.path(), json!({"viewport":[10,10]}))["ui_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let ok_ops = json!([{"op":"set_node","id":"go","patch":{"enabled":false}}]);

    let stale = edit(dir.path(), "0000", json!({"operations":ok_ops}));
    assert_eq!(error_code(&stale), "revision_conflict", "{stale}");
    assert_eq!(file(), original);

    let wrong_hash = edit(
        dir.path(),
        &current,
        json!({"operations":ok_ops,"expected_hash":"abc"}),
    );
    assert_eq!(error_code(&wrong_hash), "ui_conflict", "{wrong_hash}");
    assert_eq!(file(), original);

    for (operations, code) in [
        (
            json!([{"op":"remove_node","id":"ghost"}]),
            "ui.op.unknown_node",
        ),
        (
            json!([{"op":"move_node","id":"panel","parent":"go"}]),
            "ui.op.cycle",
        ),
        (
            json!([{"op":"add_node","id":"go","widget":"label"}]),
            "ui.op.duplicate_id",
        ),
        (
            json!([{"op":"set_style","id":"go","patch":{"opacity":9}}]),
            "ui.tree.invalid",
        ),
        (
            json!([
                {"op":"set_classes","id":"go","classes":["fine"]},
                {"op":"set_widget","id":"go","patch":{"kind":"warp"}}
            ]),
            "ui.op.widget",
        ),
    ] {
        let failed = edit(
            dir.path(),
            &current,
            json!({"operations":operations,"expected_hash":hash}),
        );
        assert_eq!(error_code(&failed), code, "{operations}: {failed}");
        assert!(
            failed["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("unchanged")
        );
        assert_eq!(file(), original, "{operations}");
    }
    // Malformed operation lists are protocol-level argument errors.
    for operations in [
        json!([{"op":"explode"}]),
        json!([{"op":"remove_node"}]),
        json!({"op":"remove_node"}),
    ] {
        let failed = edit(dir.path(), &current, json!({"operations":operations}));
        assert_eq!(failed["ok"], false, "{operations}: {failed}");
        assert_eq!(file(), original);
    }
    let missing = write(
        dir.path(),
        "ui_edit",
        json!({"scene":START,"expected_revision":current,"entity":"camera","operations":[]}),
    );
    assert_eq!(error_code(&missing), "ui_root_missing");
}

#[test]
fn ui_edit_dry_runs_and_requires_write_access() {
    let dir = fixture();
    set_ui(dir.path(), menu("Hello"));
    let original = fs::read(dir.path().join(START)).unwrap();
    let current = revision(dir.path(), START);
    let dry = edit(
        dir.path(),
        &current,
        json!({"dry_run":true,"operations":[{"op":"remove_node","id":"go"}]}),
    );
    assert_eq!(dry["ok"], true, "{dry}");
    assert_eq!(dry["saved"], false);
    assert_eq!(dry["node_count"], 2);
    assert_eq!(dry["diff"]["removed"], json!(["go"]));
    assert_eq!(fs::read(dir.path().join(START)).unwrap(), original);

    // A read-only server neither lists nor runs it.
    let read = tool_names_of(dir.path(), read_only());
    assert!(read.contains(&"ui_inspect".to_owned()));
    assert!(read.contains(&"ui_preview".to_owned()));
    assert!(!read.contains(&"ui_edit".to_owned()));
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"ui_edit","arguments":{}}),
        )],
    );
    assert_eq!(responses[0]["error"]["code"], -32602);
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
    // Required arguments are enforced before the tool runs.
    let responses = exchange(
        dir.path(),
        writable(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"ui_edit","arguments":{"scene":START,"entity":"player"}}),
        )],
    );
    assert_eq!(responses[0]["error"]["code"], -32602);
}

fn tool_names_of(root: &Path, config: engine_mcp::ServerConfig) -> Vec<String> {
    let responses = exchange(root, config, &[modern(1, "tools/list", json!({}))]);
    responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn ui_tool_schemas_are_closed_and_described() {
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        writable(),
        &[modern(1, "tools/list", json!({}))],
    );
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    for name in ["ui_inspect", "ui_preview", "ui_edit"] {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false, "{name}");
        for (property, definition) in schema["properties"].as_object().unwrap() {
            assert!(
                definition["description"].as_str().unwrap().len() > 8,
                "{name}.{property}"
            );
            assert!(definition["type"].is_string(), "{name}.{property}");
        }
        assert!(tool["description"].as_str().unwrap().len() > 100);
    }
    let edit = tools.iter().find(|t| t["name"] == "ui_edit").unwrap();
    assert_eq!(edit["annotations"]["readOnlyHint"], false);
    assert_eq!(
        edit["inputSchema"]["required"],
        json!(["scene", "expected_revision", "entity", "operations"])
    );
    let preview = tools.iter().find(|t| t["name"] == "ui_preview").unwrap();
    assert_eq!(preview["annotations"]["readOnlyHint"], true);
}
