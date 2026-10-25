//! Write-access tools: gating, optimistic concurrency, validation without
//! partial writes, atomic saves and write confinement.

mod common;

use common::*;
use engine_mcp::{Limits, ServerConfig};
use serde_json::{Value, json};
use std::{fs, path::Path};

const START: &str = "scenes/start.json";

fn tool_names(root: &Path, config: ServerConfig) -> Vec<String> {
    let responses = exchange(root, config, &[modern(1, "tools/list", json!({}))]);
    responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

const WRITE_TOOLS: [&str; 14] = [
    "scene_edit",
    "entity_create",
    "entity_update",
    "entity_delete",
    "component_set",
    "component_remove",
    "file_write",
    "asset_import_model",
    "navigation_bake",
    "acoustics_bake",
    "tilemap_create_tileset",
    "tilemap_create",
    "tilemap_define_rules",
    "tilemap_paint",
];

fn bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}

fn rev(root: &Path) -> String {
    revision(root, START)
}

fn create(root: &Path, id: &str, components: Value) -> Value {
    write(
        root,
        "entity_create",
        json!({"scene":START,"expected_revision":rev(root),"id":id,"components":components}),
    )
}

/// Asserts a failed edit: the error code and that the scene file is untouched.
fn rejected(root: &Path, response: &Value, code: &str, before: &[u8]) {
    assert_eq!(error_code(response), code, "{response}");
    assert_eq!(bytes(root, START), before, "a rejected edit must not write");
}

// ---- gating -----------------------------------------------------------------------------

#[test]
fn read_only_servers_hide_and_reject_every_write_tool() {
    let dir = fixture();
    let names = tool_names(dir.path(), read_only());
    for tool in WRITE_TOOLS {
        assert!(
            !names.contains(&tool.to_owned()),
            "{tool} must not be listed"
        );
        let responses = exchange(
            dir.path(),
            read_only(),
            &[modern(1, "tools/call", json!({"name":tool,"arguments":{}}))],
        );
        assert_eq!(responses[0]["error"]["code"], -32602);
        let message = responses[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("disabled") && message.contains("write"),
            "{message}"
        );
    }
}

#[test]
fn write_servers_list_read_tools_then_write_tools() {
    let dir = fixture();
    let names = tool_names(dir.path(), writable());
    assert_eq!(names.len(), 33);
    // Registration order: the 12 original read tools, the 8 original write
    // tools, then the navigation pair (bake writes, query only reads), then the
    // acoustics tools (inspect and query read, bake writes), then the physics pair (both read).
    let read_only_names = tool_names(dir.path(), read_only());
    assert_eq!(read_only_names.len(), 19);
    assert_eq!(names[..12], read_only_names[..12]);
    assert_eq!(names[12..20], WRITE_TOOLS[..8]);
    assert_eq!(names[20], "navigation_bake");
    assert_eq!(names[21], "navigation_query");
    assert_eq!(names[22], "acoustics_inspect");
    assert_eq!(names[23], "acoustics_query");
    assert_eq!(names[24], "acoustics_bake");
    assert_eq!(names[25], "physics_query");
    assert_eq!(names[26], "physics_debug");
    assert_eq!(read_only_names[12], "navigation_query");
    assert_eq!(read_only_names[13], "acoustics_inspect");
    assert_eq!(read_only_names[14], "acoustics_query");
    assert_eq!(read_only_names[15], "physics_query");
    assert_eq!(read_only_names[16], "physics_debug");
    // The tilemap family follows: four writers, then its two read-only tools.
    assert_eq!(names[27..31], WRITE_TOOLS[10..]);
    assert_eq!(names[31..], ["tilemap_info", "tilemap_query"]);
    assert_eq!(read_only_names[17..], ["tilemap_info", "tilemap_query"]);
    assert_eq!(WRITE_TOOLS[8], "navigation_bake");
    assert_eq!(WRITE_TOOLS[9], "acoustics_bake");
}

#[test]
fn write_tool_annotations_are_accurate() {
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        writable(),
        &[modern(1, "tools/list", json!({}))],
    );
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let expect = |name: &str, destructive: bool, idempotent: bool| {
        let tool = tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(
            tool["annotations"],
            json!({"readOnlyHint":false,"destructiveHint":destructive,
                "idempotentHint":idempotent,"openWorldHint":false}),
            "{name}"
        );
        assert!(tool["description"].as_str().unwrap().len() > 80);
        assert!(tool["outputSchema"].is_object());
    };
    expect("entity_create", false, false);
    expect("entity_update", true, true);
    expect("entity_delete", true, true);
    expect("component_set", true, true);
    expect("component_remove", true, true);
    expect("file_write", true, true);
    expect("asset_import_model", false, false);
    for tool in tools
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == true)
    {
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
}

#[test]
fn write_tools_require_their_arguments_at_the_protocol_level() {
    let dir = fixture();
    for (tool, args) in [
        ("entity_create", json!({"scene":START,"id":"x"})),
        (
            "entity_delete",
            json!({"scene":START,"expected_revision":"r"}),
        ),
        (
            "component_set",
            json!({"scene":START,"expected_revision":"r","entity":"player"}),
        ),
        ("file_write", json!({"path":"a.txt"})),
    ] {
        let responses = exchange(
            dir.path(),
            writable(),
            &[modern(
                1,
                "tools/call",
                json!({"name":tool,"arguments":args}),
            )],
        );
        assert_eq!(responses[0]["error"]["code"], -32602, "{tool}");
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("requires argument")
        );
    }
}

#[test]
fn a_provider_tool_above_the_servers_access_is_not_registered() {
    use engine_mcp::{
        AccessLevel, Annotations, Tool, ToolContext, ToolDefinition, ToolOutcome, ToolProvider,
        ToolResult,
    };
    struct Probe(AccessLevel);
    impl Tool for Probe {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: format!("probe_{}", self.0.as_str()),
                title: "Probe".into(),
                description: "Test probe that reports it ran.".into(),
                input_schema: engine_mcp::schema::object(vec![], &[]),
                output_schema: None,
                annotations: Annotations::READ_ONLY,
                access: self.0,
            }
        }
        fn call(
            &self,
            cx: &ToolContext<'_>,
            _args: &serde_json::Map<String, Value>,
        ) -> ToolOutcome {
            cx.require(self.0)?;
            Ok(ToolResult::new(json!({"ran": true})))
        }
    }
    struct Provider;
    impl ToolProvider for Provider {
        fn tools(&self) -> Vec<Box<dyn Tool>> {
            vec![
                Box::new(Probe(AccessLevel::Read)),
                Box::new(Probe(AccessLevel::Write)),
                Box::new(Probe(AccessLevel::Exec)),
            ]
        }
    }
    let dir = fixture();
    let names = |config: ServerConfig| tool_names(dir.path(), config.with_provider(Provider));
    assert!(names(read_only()).contains(&"probe_read".into()));
    assert!(!names(read_only()).contains(&"probe_write".into()));
    assert!(names(writable()).contains(&"probe_write".into()));
    assert!(!names(writable()).contains(&"probe_exec".into()));
    assert!(names(ServerConfig::new().allow_exec(true)).contains(&"probe_exec".into()));
    let ran = result_with(
        dir.path(),
        ServerConfig::new().allow_exec(true).with_provider(Provider),
        "probe_exec",
        json!({}),
    );
    assert_eq!(ran["structuredContent"], json!({"ran":true,"ok":true}));
    let responses = exchange(
        dir.path(),
        ServerConfig::new().with_provider(Provider),
        &[modern(
            1,
            "tools/call",
            json!({"name":"probe_exec","arguments":{}}),
        )],
    );
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exec")
    );
}

#[test]
fn exec_tool_names_are_reported_as_disabled_not_unknown() {
    let dir = fixture();
    for name in ["project_build", "project_run_headless", "render_screenshot"] {
        let responses = exchange(
            dir.path(),
            writable(),
            &[modern(1, "tools/call", json!({"name":name,"arguments":{}}))],
        );
        let message = responses[0]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("disabled") && message.contains("exec"),
            "{message}"
        );
    }
}

// ---- entity_create ----------------------------------------------------------------------

#[test]
fn entity_create_adds_saves_and_returns_the_new_revision() {
    let dir = fixture();
    let before = rev(dir.path());
    let created = create(
        dir.path(),
        "crate",
        json!({"Transform":{"position":[3,4,0]}}),
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["scene"], START);
    assert_eq!(created["scene_id"], "scene:start");
    assert_eq!(created["saved"], true);
    assert_eq!(created["dry_run"], false);
    assert_eq!(created["previous_revision"], before);
    assert_eq!(created["entity_count"], 4);
    assert_eq!(created["entity"]["id"], "crate");
    assert_eq!(
        created["entity"]["name"], "crate",
        "name defaults to the id"
    );
    assert_ne!(created["revision"], before);
    assert_eq!(
        created["revision"],
        rev(dir.path()),
        "the saved scene has this revision"
    );
    let on_disk: Value = serde_json::from_slice(&bytes(dir.path(), START)).unwrap();
    assert_eq!(
        on_disk["entities"][3]["components"]["Transform"]["position"],
        json!([3.0, 4.0, 0.0])
    );
    assert_eq!(data(dir.path(), "project_validate", json!({}))["ok"], true);
}

#[test]
fn entity_create_without_components_or_name_is_allowed() {
    let dir = fixture();
    let created = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"empty","name":"Empty One"}),
    );
    assert_eq!(created["entity"]["name"], "Empty One");
    assert_eq!(created["entity"]["components"], json!({}));
}

#[test]
fn consecutive_edits_chain_on_the_returned_revision() {
    let dir = fixture();
    let first = create(dir.path(), "a", json!({}));
    let second = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":first["revision"],"id":"b"}),
    );
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["previous_revision"], first["revision"]);
    assert_eq!(second["entity_count"], 5);
}

#[test]
fn entity_create_rejects_duplicates_and_invalid_content_without_writing() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let cases = [
        (
            create(dir.path(), "player", json!({})),
            "scene.entity.id.duplicate",
        ),
        (
            create(dir.path(), "x", json!({"Rigidbody":{}})),
            "component_invalid",
        ),
        (
            create(dir.path(), "x", json!({"Transform":{"position":[1,2]}})),
            "component_invalid",
        ),
        (
            create(
                dir.path(),
                "x",
                json!({"Transform":{"position":[0,0,0],"bogus":1}}),
            ),
            "component_invalid",
        ),
        (
            create(dir.path(), "x", json!({"Script":{"asset":"asset:ghost"}})),
            "project.asset.unknown",
        ),
        (
            create(dir.path(), "x", json!({"Script":{"asset":"asset:texture"}})),
            "project.asset.kind",
        ),
        (
            create(
                dir.path(),
                "x",
                json!({"SpriteRenderer":{"texture":"asset:texture"}}),
            ),
            "scene.component.invalid",
        ),
        (
            create(dir.path(), "x", json!({"Camera2D":{}})),
            "scene.camera.multiple",
        ),
        (
            create(dir.path(), "", json!({})),
            "scene.entity.id.duplicate",
        ),
        (
            create(
                dir.path(),
                "x",
                json!({"Transform":{"position":[1e40,0,0]}}),
            ),
            "scene.component.invalid",
        ),
    ];
    for (response, code) in &cases {
        rejected(dir.path(), response, code, &original);
        assert!(response["error"]["hint"].is_string(), "{response}");
        assert!(
            !response["diagnostics"].as_array().unwrap().is_empty(),
            "{response}"
        );
    }
}

#[test]
fn stale_revisions_are_conflicts_and_change_nothing() {
    let dir = fixture();
    let stale = rev(dir.path());
    create(dir.path(), "first", json!({}));
    let original = bytes(dir.path(), START);
    let response = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":stale,"id":"second"}),
    );
    rejected(dir.path(), &response, "revision_conflict", &original);
    assert!(
        response["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("Re-read")
    );
    assert!(
        !response["error"].to_string().contains(&rev(dir.path())),
        "no free retry token"
    );
    let bogus = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":"nope","id":"third"}),
    );
    rejected(dir.path(), &bogus, "revision_conflict", &original);
}

#[test]
fn an_external_change_between_read_and_edit_is_a_conflict() {
    let dir = fixture();
    let seen = rev(dir.path());
    let mut scene: Value = serde_json::from_slice(&bytes(dir.path(), START)).unwrap();
    scene["entities"][0]["name"] = json!("Changed Elsewhere");
    fs::write(dir.path().join(START), scene.to_string()).unwrap();
    let original = bytes(dir.path(), START);
    let response = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":seen,"id":"late"}),
    );
    rejected(dir.path(), &response, "revision_conflict", &original);
}

#[test]
fn dry_runs_validate_and_report_without_saving() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let before = rev(dir.path());
    let dry = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":before,"id":"ghost","dry_run":true}),
    );
    assert_eq!(dry["ok"], true);
    assert_eq!(dry["saved"], false);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(bytes(dir.path(), START), original);
    let real = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":before,"id":"ghost"}),
    );
    assert_eq!(
        real["revision"], dry["revision"],
        "a dry run predicts the real revision"
    );
    let bad = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":real["revision"],"id":"ghost","dry_run":true}),
    );
    assert_eq!(error_code(&bad), "scene.entity.id.duplicate");
}

#[test]
fn edits_leave_no_temporary_files_behind() {
    let dir = fixture();
    create(dir.path(), "tidy", json!({}));
    let mut names: Vec<_> = fs::read_dir(dir.path().join("scenes"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["second.json", "start.json"]);
}

#[test]
fn edits_apply_to_any_scene_and_leave_the_others_alone() {
    let dir = fixture();
    let startup = bytes(dir.path(), START);
    let second = revision(dir.path(), "scenes/second.json");
    let created = write(
        dir.path(),
        "entity_create",
        json!({"scene":"scenes/second.json","expected_revision":second,"id":"extra",
            "components":{"Transform":{"position":[9,9,0]}}}),
    );
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["scene_id"], "scene:second");
    assert_eq!(bytes(dir.path(), START), startup);
    let listing = data(dir.path(), "scene_list", json!({}));
    assert_eq!(listing["scenes"][1]["entities"], 2);
}

#[test]
fn scene_edits_use_clean_numbers_on_disk() {
    let dir = fixture();
    create(
        dir.path(),
        "decimal",
        json!({"Transform":{"position":[0.1,0.2,0.3]}}),
    );
    let text = String::from_utf8(bytes(dir.path(), START)).unwrap();
    assert!(text.contains("0.1,") || text.contains("0.1\n"), "{text}");
    assert!(!text.contains("0.10000000"), "{text}");
}

// ---- entity_update ----------------------------------------------------------------------

#[test]
fn entity_update_renames_and_replaces_components() {
    let dir = fixture();
    let renamed = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"npc-less-mesh","name":"x"}),
    );
    assert_eq!(error_code(&renamed), "entity_not_found");
    let renamed = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"mesh","name":"Renamed"}),
    );
    assert_eq!(renamed["entity"]["name"], "Renamed");
    assert!(
        renamed["entity"]["components"]["MeshRenderer"].is_object(),
        "components kept"
    );
    let replaced = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":renamed["revision"],"id":"mesh",
            "components":{"Transform":{"position":[7,7,7]}}}),
    );
    assert_eq!(replaced["entity"]["name"], "Renamed");
    assert_eq!(
        replaced["entity"]["components"],
        json!({"Transform":{"version":1,"position":[7.0,7.0,7.0]}})
    );
}

#[test]
fn entity_update_requires_something_to_change_and_validates() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let nothing = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"mesh"}),
    );
    rejected(dir.path(), &nothing, "invalid_argument", &original);
    let bad = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"player",
            "components":{"Transform":{"position":[0,0,0]},"Script":{"asset":"asset:ghost"}}}),
    );
    rejected(dir.path(), &bad, "project.asset.unknown", &original);
    let orphaned = write(
        dir.path(),
        "entity_update",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"player","components":{}}),
    );
    rejected(dir.path(), &orphaned, "scene.camera.follow", &original);
}

// ---- entity_delete ----------------------------------------------------------------------

#[test]
fn entity_delete_removes_the_entity() {
    let dir = fixture();
    let deleted = write(
        dir.path(),
        "entity_delete",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"mesh"}),
    );
    assert_eq!(deleted["ok"], true, "{deleted}");
    assert_eq!(deleted["entity"], Value::Null);
    assert_eq!(deleted["entity_count"], 2);
    let missing = data(dir.path(), "entity_get", json!({"scene":START,"id":"mesh"}));
    assert_eq!(error_code(&missing), "entity_not_found");
}

#[test]
fn entity_delete_rejects_invalidating_and_unknown_deletes() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let followed = write(
        dir.path(),
        "entity_delete",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"player"}),
    );
    rejected(dir.path(), &followed, "scene.camera.follow", &original);
    let unknown = write(
        dir.path(),
        "entity_delete",
        json!({"scene":START,"expected_revision":rev(dir.path()),"id":"nobody"}),
    );
    rejected(dir.path(), &unknown, "entity_not_found", &original);
}

// ---- component_set / component_remove ---------------------------------------------------

#[test]
fn component_set_replaces_a_component_with_defaults() {
    let dir = fixture();
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
            "component":"SpriteRenderer","value":{"texture":"asset:texture","layer":3}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let sprite = &set["entity"]["components"]["SpriteRenderer"];
    assert_eq!(sprite["layer"], 3);
    assert_eq!(
        sprite["size"],
        json!([1.0, 1.0]),
        "omitted fields take defaults"
    );
}

#[test]
fn component_set_merge_overlays_fields() {
    let dir = fixture();
    write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
            "component":"SpriteRenderer","value":{"texture":"asset:texture","size":[2,3]}}),
    );
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
            "component":"SpriteRenderer","value":{"tint":[1,0,0,1]},"merge":true}),
    );
    let sprite = &merged["entity"]["components"]["SpriteRenderer"];
    assert_eq!(sprite["tint"], json!([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(
        sprite["size"],
        json!([2.0, 3.0]),
        "merge keeps the other fields"
    );
    assert_eq!(sprite["texture"], "asset:texture");
}

#[test]
fn component_set_adds_new_components_and_merge_on_absent_acts_like_set() {
    let dir = fixture();
    let added = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"mesh",
            "component":"Transform","value":{"position":[1,1,1]},"merge":true}),
    );
    assert_eq!(
        added["entity"]["components"]["Transform"]["position"],
        json!([1.0, 1.0, 1.0])
    );
}

#[test]
fn component_set_rejects_bad_values_without_writing() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let set = |component: &str, value: Value| {
        write(
            dir.path(),
            "component_set",
            json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
                "component":component,"value":value}),
        )
    };
    rejected(
        dir.path(),
        &set("Transform", json!({"position":[1,2]})),
        "component_invalid",
        &original,
    );
    rejected(
        dir.path(),
        &set("Transform", json!({"position":[0,0,0],"bogus":1})),
        "component_invalid",
        &original,
    );
    rejected(
        dir.path(),
        &set("Collider2D", json!({"size":[0,1]})),
        "scene.component.invalid",
        &original,
    );
    rejected(
        dir.path(),
        &set("Script", json!({"asset":"asset:mesh"})),
        "project.asset.kind",
        &original,
    );
    rejected(
        dir.path(),
        &set("Rigidbody", json!({})),
        "invalid_argument",
        &original,
    );
    let not_object = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player",
            "component":"Transform","value":[1,2,3]}),
    );
    rejected(dir.path(), &not_object, "invalid_argument", &original);
    let missing = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"nobody",
            "component":"Transform","value":{"position":[0,0,0]}}),
    );
    rejected(dir.path(), &missing, "entity_not_found", &original);
}

#[test]
fn component_remove_removes_and_is_idempotent_for_absent_components() {
    let dir = fixture();
    let removed = write(
        dir.path(),
        "component_remove",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player","component":"Collider2D"}),
    );
    assert_eq!(removed["ok"], true, "{removed}");
    assert!(removed["entity"]["components"].get("Collider2D").is_none());
    let again = write(
        dir.path(),
        "component_remove",
        json!({"scene":START,"expected_revision":removed["revision"],"entity":"player","component":"Collider2D"}),
    );
    assert_eq!(again["ok"], true);
    assert_eq!(
        again["revision"], removed["revision"],
        "no change, same revision"
    );
}

#[test]
fn component_remove_rejects_invalidating_removals() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let response = write(
        dir.path(),
        "component_remove",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player","component":"Transform"}),
    );
    rejected(dir.path(), &response, "scene.component.invalid", &original);
}

#[test]
fn edits_to_a_broken_project_are_rejected_with_diagnostics() {
    let dir = fixture();
    let revision = rev(dir.path());
    fs::remove_file(dir.path().join("assets/white.ppm")).unwrap();
    let original = bytes(dir.path(), START);
    let response = write(
        dir.path(),
        "entity_create",
        json!({"scene":START,"expected_revision":revision,"id":"x"}),
    );
    rejected(dir.path(), &response, "project.asset.missing", &original);
}

// ---- scene path confinement for edits ---------------------------------------------------

#[test]
fn edits_cannot_reach_outside_the_project() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    let scene = fs::read(dir.path().join("scenes/second.json")).unwrap();
    fs::write(outside.path().join("second.json"), &scene).unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    for path in [
        "../second.json",
        "/etc/x.json",
        "escape/second.json",
        "scenes/../../x.json",
    ] {
        let response = write(
            dir.path(),
            "entity_create",
            json!({"scene":path,"expected_revision":"r","id":"x"}),
        );
        assert_eq!(error_code(&response), "path_invalid", "{path}");
    }
    assert_eq!(fs::read(outside.path().join("second.json")).unwrap(), scene);
}

// ---- file_write -------------------------------------------------------------------------

fn put(root: &Path, path: &str, content: &str, expected: Option<&str>) -> Value {
    let mut args = json!({"path":path,"content":content});
    if let Some(hash) = expected {
        args["expected_hash"] = json!(hash);
    }
    write(root, "file_write", args)
}

#[test]
fn file_write_creates_files_and_directories_atomically() {
    let dir = fixture();
    let created = put(dir.path(), "docs/deep/notes.txt", "hello\nworld\n", None);
    assert_eq!(created["ok"], true, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["bytes"], 12);
    assert_eq!(created["validated"], "none");
    assert_eq!(created["path"], "docs/deep/notes.txt");
    assert_eq!(
        fs::read_to_string(dir.path().join("docs/deep/notes.txt")).unwrap(),
        "hello\nworld\n"
    );
    let read = data(
        dir.path(),
        "file_read",
        json!({"path":"docs/deep/notes.txt"}),
    );
    assert_eq!(read["hash"], created["hash"]);
    let tmp: Vec<_> = fs::read_dir(dir.path().join("docs/deep"))
        .unwrap()
        .collect();
    assert_eq!(tmp.len(), 1, "no temporary files remain");
}

#[test]
fn file_write_overwrite_needs_the_current_hash() {
    let dir = fixture();
    let hash = data(dir.path(), "file_read", json!({"path":"notes/readme.md"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let original = bytes(dir.path(), "notes/readme.md");
    let missing = put(dir.path(), "notes/readme.md", "new", None);
    assert_eq!(error_code(&missing), "hash_required");
    let wrong = put(dir.path(), "notes/readme.md", "new", Some(&"0".repeat(64)));
    assert_eq!(error_code(&wrong), "revision_conflict");
    assert_eq!(bytes(dir.path(), "notes/readme.md"), original);
    let ok = put(dir.path(), "notes/readme.md", "new text\n", Some(&hash));
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["created"], false);
    assert_eq!(ok["previous_hash"], hash);
    assert_ne!(ok["hash"], hash);
    assert_eq!(
        fs::read_to_string(dir.path().join("notes/readme.md")).unwrap(),
        "new text\n"
    );
    let stale = put(dir.path(), "notes/readme.md", "again", Some(&hash));
    assert_eq!(
        error_code(&stale),
        "revision_conflict",
        "the old hash is stale now"
    );
}

#[test]
fn file_write_expected_hash_on_a_missing_file_is_an_error() {
    let dir = fixture();
    let response = put(dir.path(), "nothing.txt", "x", Some("abc"));
    assert_eq!(error_code(&response), "file_not_found");
    assert!(!dir.path().join("nothing.txt").exists());
}

#[test]
fn file_write_enforces_size_and_text_rules() {
    let dir = fixture();
    let config = ServerConfig::new().allow_write(true).with_limits(Limits {
        max_file_write_bytes: 8,
        ..Default::default()
    });
    let big = result_with(
        dir.path(),
        config,
        "file_write",
        json!({"path":"big.txt","content":"123456789"}),
    )["structuredContent"]
        .clone();
    assert_eq!(error_code(&big), "content_too_large");
    assert!(!dir.path().join("big.txt").exists());
    let nul = put(dir.path(), "nul.txt", "a\u{0}b", None);
    assert_eq!(error_code(&nul), "not_text");
    let binary = put(dir.path(), "assets/blob.bin", "text", Some("whatever"));
    assert_eq!(error_code(&binary), "not_text");
    assert_eq!(bytes(dir.path(), "assets/blob.bin"), [0, 1, 2, 255, 0, 7]);
    let not_string = write(
        dir.path(),
        "file_write",
        json!({"path":"x.txt","content":5}),
    );
    assert_eq!(error_code(&not_string), "invalid_argument");
}

#[test]
fn file_write_refuses_escapes_and_protected_directories() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    for path in [
        "../escape.txt",
        "a/../../escape.txt",
        "/tmp/escape.txt",
        "C:/escape.txt",
        "notes\\x.txt",
        ".git/config",
        ".GIT/hooks/pre-commit",
        "notes/",
        "",
        ".",
        "notes",
    ] {
        let response = put(dir.path(), path, "x", None);
        assert_eq!(response["ok"], false, "{path}: {response}");
    }
    for path in [".derived/cache.txt", "target/out.txt", "TARGET/out.txt"] {
        assert_eq!(
            error_code(&put(dir.path(), path, "x", None)),
            "path_protected",
            "{path}"
        );
    }
    assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    assert!(!dir.path().join(".git").exists());
}

#[test]
fn file_write_never_follows_links() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    for path in ["escape/new.txt", "escape/deeper/new.txt"] {
        let response = put(dir.path(), path, "pwned", None);
        assert_eq!(response["ok"], false, "{path}: {response}");
    }
    assert!(
        fs::read_dir(outside.path()).unwrap().next().is_none(),
        "nothing written outside"
    );
    fs::write(outside.path().join("target.txt"), "original").unwrap();
    if file_link(
        &outside.path().join("target.txt"),
        &dir.path().join("notes/link.txt"),
    ) {
        let response = put(dir.path(), "notes/link.txt", "pwned", Some("x"));
        assert_eq!(error_code(&response), "path_link");
        let response = put(dir.path(), "notes/link.txt", "pwned", None);
        assert_eq!(error_code(&response), "path_link");
        assert_eq!(
            fs::read_to_string(outside.path().join("target.txt")).unwrap(),
            "original"
        );
    }
}

#[test]
fn file_write_validates_scenes_before_writing() {
    let dir = fixture();
    let original = bytes(dir.path(), START);
    let hash = data(dir.path(), "file_read", json!({"path":START}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let unknown_asset = json!({"schema":1,"scene_id":"scene:start","entities":[
        {"id":"e","name":"E","components":{"Script":{"asset":"asset:ghost"}}}]})
    .to_string();
    let response = put(dir.path(), START, &unknown_asset, Some(&hash));
    rejected(dir.path(), &response, "project.asset.unknown", &original);
    let not_a_scene = put(dir.path(), START, "{\"hello\":1}", Some(&hash));
    rejected(dir.path(), &not_a_scene, "scene_invalid", &original);
    let broken_json = put(dir.path(), START, "{", Some(&hash));
    rejected(dir.path(), &broken_json, "scene_invalid", &original);
    let valid = json!({"schema":1,"scene_id":"scene:start","entities":[
        {"id":"solo","name":"Solo","components":{"Transform":{"position":[1,2,3]}}}]})
    .to_string();
    let ok = put(dir.path(), START, &valid, Some(&hash));
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["validated"], "scene");
    assert_eq!(
        ok["revision"],
        rev(dir.path()),
        "the reported revision matches scene_inspect"
    );
}

#[test]
fn file_write_restores_the_manifest_when_it_would_break_a_valid_project() {
    let dir = fixture();
    let original = bytes(dir.path(), "project.json");
    let hash = data(dir.path(), "file_read", json!({"path":"project.json"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["startup_scene"] = json!("scenes/missing.json");
    let response = put(
        dir.path(),
        "project.json",
        &manifest.to_string(),
        Some(&hash),
    );
    assert_eq!(error_code(&response), "project.asset.missing");
    assert_eq!(response["error"]["details"]["rolled_back"], true);
    assert_eq!(
        bytes(dir.path(), "project.json"),
        original,
        "the manifest was restored"
    );
    assert_eq!(data(dir.path(), "project_validate", json!({}))["ok"], true);

    manifest["startup_scene"] = json!("scenes/start.json");
    manifest["name"] = json!("Renamed Fixture");
    let ok = put(
        dir.path(),
        "project.json",
        &manifest.to_string(),
        Some(&hash),
    );
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["validated"], "manifest");
    assert_eq!(
        data(dir.path(), "project_info", json!({}))["name"],
        "Renamed Fixture"
    );
}

#[test]
fn file_write_can_add_a_scene_then_edit_it() {
    let dir = fixture();
    let scene = json!({"schema":1,"scene_id":"scene:new","entities":[]}).to_string();
    assert_eq!(put(dir.path(), "scenes/new.json", &scene, None)["ok"], true);
    let created = write(
        dir.path(),
        "entity_create",
        json!({"scene":"scenes/new.json","expected_revision":revision(dir.path(), "scenes/new.json"),
            "id":"first"}),
    );
    assert_eq!(created["ok"], true, "{created}");
}

#[test]
fn file_write_results_are_searchable_and_listed() {
    let dir = fixture();
    put(
        dir.path(),
        "src/extra.rs",
        "pub fn needle_function() {}\n",
        None,
    );
    let search = data(
        dir.path(),
        "search_text",
        json!({"query":"needle_function"}),
    );
    assert_eq!(search["matches"][0]["path"], "src/extra.rs");
    let listing = data(dir.path(), "file_list", json!({"extension":"rs"}));
    assert_eq!(listing["total"], 2);
}

#[test]
fn custom_schema_discovery_set_validation_and_transactional_parent_edit_share_model() {
    let dir = fixture();
    let manifest = dir.path().join("project.json");
    let mut project: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    project["componentSchemas"] = json!("components.json");
    fs::write(&manifest, serde_json::to_vec(&project).unwrap()).unwrap();
    fs::write(dir.path().join("components.json"),json!([{"name":"game.Health","version":1,"schema":{"type":"object","properties":{"hp":{"type":"integer","minimum":0}},"required":["hp"],"additionalProperties":false},"defaults":{"hp":10},"dataOnly":true}]).to_string()).unwrap();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"game.Health"}),
    );
    assert_eq!(schema["components"][0]["name"], "game.Health");
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"player","component":"game.Health","value":{"version":1,"data":{"hp":7}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    assert_eq!(
        set["entity"]["components"]["custom"]["game.Health"]["data"]["hp"],
        7
    );
    let before = bytes(dir.path(), START);
    let invalid = write(
        dir.path(),
        "scene_edit",
        json!({"scene":START,"expected_revision":rev(dir.path()),"operations":[{"op":"set_position","entity_id":"player","position":[9,9,0]},{"op":"reparent","entity_id":"player","parent":"player"}]}),
    );
    assert_eq!(invalid["ok"], false);
    assert_eq!(bytes(dir.path(), START), before);
    let edited = write(
        dir.path(),
        "scene_edit",
        json!({"scene":START,"expected_revision":rev(dir.path()),"operations":[{"op":"reparent","entity_id":"player","parent":"camera"}]}),
    );
    assert_eq!(edited["ok"], true, "{edited}");
    let got = data(
        dir.path(),
        "entity_get",
        json!({"scene":START,"id":"player"}),
    );
    assert_eq!(got["entity"]["parent"], "camera");
}

// ---- upscaler setting -------------------------------------------------------------------

#[test]
fn component_set_edits_the_camera_upscaler_request() {
    let dir = fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"Camera3D"}),
    );
    let upscaler = &schema["components"][0]["schema"]["properties"]["upscaler"];
    assert_eq!(
        upscaler["properties"]["mode"]["enum"],
        json!(["none", "dlss_sr"])
    );
    assert_eq!(
        upscaler["properties"]["quality"]["enum"],
        json!([
            "ultra_performance",
            "performance",
            "balanced",
            "quality",
            "dlaa"
        ])
    );
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"Camera3D","value":{"upscaler":{"mode":"dlss_sr"}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    assert_eq!(
        set["entity"]["components"]["Camera3D"]["upscaler"],
        json!({"mode":"dlss_sr","quality":"quality"})
    );
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"Camera3D","value":{"upscaler":{"mode":"dlss_sr","quality":"dlaa"}},
            "merge":true}),
    );
    assert_eq!(
        merged["entity"]["components"]["Camera3D"]["upscaler"]["quality"],
        "dlaa"
    );
    let saved: Value = serde_json::from_slice(&bytes(dir.path(), START)).unwrap();
    let camera = saved["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "camera")
        .unwrap();
    assert_eq!(
        camera["components"]["Camera3D"]["upscaler"],
        json!({"mode":"dlss_sr","quality":"dlaa"})
    );
    let original = bytes(dir.path(), START);
    let bad = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"Camera3D","value":{"upscaler":{"mode":"dlss5"}}}),
    );
    rejected(dir.path(), &bad, "component_invalid", &original);
}

// ---- lights -----------------------------------------------------------------------------

#[test]
fn component_set_adds_edits_and_validates_light_components() {
    let dir = fixture();
    let schema = data(
        dir.path(),
        "component_schema",
        json!({"component":"SpotLight"}),
    );
    let properties = &schema["components"][0]["schema"]["properties"];
    assert_eq!(
        properties["shadows"]["properties"]["filter"]["enum"],
        json!(["hard", "pcf", "pcss"])
    );
    assert_eq!(properties["lumens"]["default"], 800);
    let set = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"SpotLight","value":{"lumens":1200,"range":9,
            "shadows":{"enabled":true,"filter":"pcss","source_radius":0.2}}}),
    );
    assert_eq!(set["ok"], true, "{set}");
    let spot = &set["entity"]["components"]["SpotLight"];
    assert_eq!(spot["lumens"], 1200.0);
    assert_eq!(spot["shadows"]["filter"], "pcss");
    let merged = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"SpotLight","value":{"outer_angle_radians":0.8},"merge":true}),
    );
    assert_eq!(merged["ok"], true, "{merged}");
    assert_eq!(
        merged["entity"]["components"]["SpotLight"]["outer_angle_radians"],
        0.8
    );
    assert_eq!(
        merged["entity"]["components"]["SpotLight"]["lumens"],
        1200.0
    );
    let original = bytes(dir.path(), START);
    let bad = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"SpotLight","value":{"inner_angle_radians":0.9,
            "outer_angle_radians":0.5}}),
    );
    rejected(dir.path(), &bad, "light.spot.invalid", &original);
    let bad = write(
        dir.path(),
        "component_set",
        json!({"scene":START,"expected_revision":rev(dir.path()),"entity":"camera",
            "component":"DirectionalLight","value":{"shadows":{"cascades":9}}}),
    );
    rejected(dir.path(), &bad, "light.shadows.invalid", &original);
}
