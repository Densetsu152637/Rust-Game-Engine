//! Project manifest loading: diagnostics, confinement, stable asset catalogue,
//! input bindings and external Rust crate selection.
use super::common::*;
use engine_project::{Project, schema};
use serde_json::{Value, json};
use std::{fs, path::Path, path::PathBuf};

fn fails(mutate: impl FnOnce(&mut Proj), expected: &str) {
    let mut p = Proj::new();
    mutate(&mut p);
    expect_code(p.load(), expected);
}

fn raw_manifest(content: &str, expected: &str) {
    let p = Proj::new();
    fs::write(p.manifest_path(), content).unwrap();
    expect_code(Project::load(p.manifest_path()), expected);
}

#[test]
fn valid_project_exposes_name_schema_and_startup_scene() {
    let p = Proj::new();
    let project = p.project();
    assert_eq!(project.schema, 1);
    assert_eq!(project.name, "Behaviour Project");
    assert_eq!(project.startup_scene, PathBuf::from("scene.json"));
    assert_eq!(project.scene.id, "room:a");
    assert_eq!(project.scene.source, PathBuf::from("scene.json"));
}

#[test]
fn valid_project_root_is_the_canonical_manifest_directory() {
    let p = Proj::new();
    let project = p.project();
    assert_eq!(project.root, fs::canonicalize(p.root()).unwrap());
    assert_eq!(
        project.manifest,
        fs::canonicalize(p.manifest_path()).unwrap()
    );
}

#[test]
fn asset_catalogue_is_ordered_by_stable_id() {
    let p = Proj::new();
    let ids: Vec<_> = p.project().assets.keys().cloned().collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    assert_eq!(ids.len(), 9);
}

#[test]
fn asset_declaration_order_does_not_change_the_catalogue() {
    let mut p = Proj::new();
    let before = p.project().assets;
    p.manifest["assets"].as_array_mut().unwrap().reverse();
    assert_eq!(p.project().assets, before);
}

#[test]
fn nested_manifest_directory_becomes_the_confinement_root() {
    let p = Proj::new();
    p.write(
        "game/s.json",
        serde_json::to_vec(&scene_json("n", vec![marker("m", 1.0, 2.0)])).unwrap(),
    );
    p.write(
        "game/project.json",
        r#"{"schema":1,"name":"Nested","startup_scene":"s.json","assets":[]}"#,
    );
    let nested = Project::load(p.root().join("game/project.json")).unwrap();
    assert_eq!(
        nested.root,
        fs::canonicalize(p.root().join("game")).unwrap()
    );
    assert_eq!(nested.scene.entities[0].id, "m");
}

#[test]
fn nested_manifest_cannot_reference_the_parent_directory() {
    let p = Proj::new();
    p.write(
        "game/s.json",
        serde_json::to_vec(&scene_json("n", vec![])).unwrap(),
    );
    p.write(
        "game/project.json",
        r#"{"schema":1,"name":"Nested","startup_scene":"s.json","assets":[{"id":"x","kind":"script","path":"../player.rs"}]}"#,
    );
    expect_code(
        Project::load(p.root().join("game/project.json")),
        "project.path.outside_root",
    );
}

#[test]
fn missing_manifest_reports_file_io() {
    let p = Proj::new();
    expect_code(
        Project::load(p.root().join("absent.json")),
        "project.file.io",
    );
}

#[test]
fn directory_in_place_of_manifest_reports_file_io() {
    let p = Proj::new();
    fs::create_dir(p.root().join("dir.json")).unwrap();
    expect_code(Project::load(p.root().join("dir.json")), "project.file.io");
}

#[test]
fn oversized_manifest_is_refused_before_parsing() {
    let p = Proj::new();
    fs::write(p.manifest_path(), vec![b' '; 16 * 1024 * 1024 + 1]).unwrap();
    expect_code(Project::load(p.manifest_path()), "project.file.too_large");
}

table! { raw_manifest;
    manifest_truncated_object_is_malformed => ("{", "project.document.malformed");
    manifest_empty_file_is_malformed => ("", "project.document.malformed");
    manifest_array_root_is_malformed => ("[]", "project.document.malformed");
    manifest_null_root_is_malformed => ("null", "project.document.malformed");
    manifest_number_root_is_malformed => ("42", "project.document.malformed");
    manifest_bom_prefix_is_malformed => ("\u{feff}{}", "project.document.malformed");
    manifest_trailing_comma_is_malformed => (r#"{"schema":1,}"#, "project.document.malformed");
    manifest_empty_object_lacks_required_fields => ("{}", "project.document.malformed");
    manifest_trailing_garbage_is_malformed => (r#"{"schema":1,"name":"a","startup_scene":"s","assets":[]} x"#, "project.document.malformed");
}

table! { fails;
    manifest_unknown_field_is_rejected => (|p| p.manifest["extra"] = json!(true), "project.document.malformed");
    manifest_missing_name_is_rejected => (|p| { p.manifest.as_object_mut().unwrap().remove("name"); }, "project.document.malformed");
    manifest_missing_assets_is_rejected => (|p| { p.manifest.as_object_mut().unwrap().remove("assets"); }, "project.document.malformed");
    manifest_missing_startup_scene_is_rejected => (|p| { p.manifest.as_object_mut().unwrap().remove("startup_scene"); }, "project.document.malformed");
    manifest_missing_schema_is_rejected => (|p| { p.manifest.as_object_mut().unwrap().remove("schema"); }, "project.document.malformed");
    manifest_schema_string_is_rejected => (|p| p.manifest["schema"] = json!("1"), "project.document.malformed");
    manifest_schema_zero_is_unsupported => (|p| p.manifest["schema"] = json!(0), "project.schema.unsupported");
    manifest_schema_two_is_unsupported => (|p| p.manifest["schema"] = json!(2), "project.schema.unsupported");
    manifest_schema_max_u32_is_unsupported => (|p| p.manifest["schema"] = json!(u32::MAX), "project.schema.unsupported");
    manifest_schema_negative_is_malformed => (|p| p.manifest["schema"] = json!(-1), "project.document.malformed");
    manifest_name_empty_is_invalid => (|p| p.manifest["name"] = json!(""), "project.invalid");
    manifest_name_with_newline_is_invalid => (|p| p.manifest["name"] = json!("a\nb"), "project.invalid");
    manifest_name_with_tab_is_invalid => (|p| p.manifest["name"] = json!("a\tb"), "project.invalid");
    manifest_name_over_256_bytes_is_invalid => (|p| p.manifest["name"] = json!("n".repeat(257)), "project.invalid");
    manifest_assets_wrong_type_is_malformed => (|p| p.manifest["assets"] = json!({}), "project.document.malformed");
    asset_duplicate_id_is_rejected => (|p| p.add_asset("player", "script", "other.rs"), "project.asset.id.duplicate");
    asset_empty_id_is_rejected => (|p| p.add_asset("", "texture", "white.ppm"), "project.asset.id.duplicate");
    asset_overlong_id_is_rejected => (|p| p.add_asset(&"i".repeat(257), "script", "other.rs"), "project.asset.id.duplicate");
    asset_control_character_id_is_rejected => (|p| p.add_asset("a\u{7}b", "script", "other.rs"), "project.asset.id.duplicate");
    asset_unknown_kind_is_rejected => (|p| p.add_asset("v", "unknown_asset_kind", "white.ppm"), "project.asset.kind");
    asset_kind_is_case_sensitive => (|p| p.add_asset("v", "Texture", "white.ppm"), "project.asset.kind");
    asset_empty_kind_is_rejected => (|p| p.add_asset("v", "", "white.ppm"), "project.asset.kind");
    asset_unknown_field_is_rejected => (|p| p.manifest["assets"][0]["extra"] = json!(1), "project.document.malformed");
    asset_missing_path_is_rejected => (|p| { p.manifest["assets"][0].as_object_mut().unwrap().remove("path"); }, "project.document.malformed");
    asset_parent_traversal_is_rejected => (|p| p.add_asset("x", "script", "../outside.rs"), "project.path.outside_root");
    asset_deep_parent_traversal_is_rejected => (|p| p.add_asset("x", "script", "a/../../outside.rs"), "project.path.outside_root");
    asset_traversal_that_returns_inside_is_still_rejected => (|p| p.add_asset("x", "script", "sub/../other2.rs"), "project.path.outside_root");
    asset_empty_path_is_rejected => (|p| p.add_asset("x", "script", ""), "project.path.outside_root");
    asset_missing_file_is_reported => (|p| p.add_asset("x", "script", "nothing.rs"), "project.asset.missing");
    asset_directory_is_not_a_file => (|p| { fs::create_dir(p.root().join("folder.rs")).unwrap(); p.add_asset("x", "script", "folder.rs"); }, "project.path.outside_root");
    asset_duplicate_path_is_rejected => (|p| p.add_asset("again", "script", "player.rs"), "project.asset.path.duplicate");
    asset_duplicate_path_through_dot_segment_is_rejected => (|p| p.add_asset("again", "script", "./player.rs"), "project.asset.path.duplicate");
    asset_duplicate_path_with_other_kind_is_rejected => (|p| p.add_asset("again", "texture", "white.ppm"), "project.asset.path.duplicate");
    script_with_txt_extension_is_rejected => (|p| { p.write("s.txt", "x"); p.add_asset("s", "script", "s.txt"); }, "project.script.language");
    script_with_lua_extension_is_rejected => (|p| { p.write("s.lua", "return"); p.add_asset("s", "script", "s.lua"); }, "project.script.language");
    script_without_extension_is_rejected => (|p| { p.write("script", "x"); p.add_asset("s", "script", "script"); }, "project.script.language");
    script_uppercase_extension_is_rejected => (|p| { p.write("s.RS", "x"); p.add_asset("s", "script", "s.RS"); }, "project.script.language");
    script_over_one_mebibyte_is_rejected => (|p| p.write("player.rs", vec![b'x'; 1024 * 1024 + 1]), "project.file.too_large");
    mesh_with_bad_header_fails_project_validation => (|p| p.write("quad.mesh", "CGMESH 9\n"), "asset.mesh.invalid");
    texture_with_garbage_fails_project_validation => (|p| p.write("white.ppm", "not an image"), "asset.texture.invalid");
    font_with_bad_json_fails_project_validation => (|p| p.write("font.json", "{"), "project.document.malformed");
    font_atlas_with_unknown_texture_asset_is_rejected => (|p| { let mut f = font_json(); f["texture"] = json!("nowhere"); p.write("font.json", serde_json::to_vec(&f).unwrap()); }, "project.asset.unknown");
    font_atlas_texture_of_wrong_kind_is_rejected => (|p| { let mut f = font_json(); f["texture"] = json!("mesh"); p.write("font.json", serde_json::to_vec(&f).unwrap()); }, "project.asset.kind");
    font_glyph_outside_texture_is_rejected => (|p| { let mut f = font_json(); f["glyphs"][1]["source"] = json!([6, 0, 4, 4]); p.write("font.json", serde_json::to_vec(&f).unwrap()); }, "asset.font.invalid");
    startup_scene_missing_file_is_reported => (|p| p.manifest["startup_scene"] = json!("gone.json"), "project.asset.missing");
    startup_scene_traversal_is_rejected => (|p| p.manifest["startup_scene"] = json!("../scene.json"), "project.path.outside_root");
    startup_scene_empty_path_is_rejected => (|p| p.manifest["startup_scene"] = json!(""), "project.path.outside_root");
    startup_scene_directory_is_rejected => (|p| { fs::create_dir(p.root().join("rooms")).unwrap(); p.manifest["startup_scene"] = json!("rooms"); }, "project.path.outside_root");
    startup_scene_not_json_is_malformed => (|p| { p.write("alt.json", "scene"); p.manifest["startup_scene"] = json!("alt.json"); }, "project.document.malformed");
    declared_scene_asset_with_duplicate_entities_fails_load => (|p| p.add_scene("scene:bad", "bad.json", scene_json("bad", vec![marker("x", 0.0, 0.0), marker("x", 1.0, 1.0)])), "scene.entity.id.duplicate");
    declared_scene_asset_with_unknown_script_fails_load => (|p| p.add_scene("scene:bad", "bad.json", scene_json("bad", vec![ent("x", json!({"Script":{"asset":"ghost"}}))])), "project.asset.unknown");
    declared_scene_asset_script_of_wrong_kind_fails_load => (|p| p.add_scene("scene:bad", "bad.json", scene_json("bad", vec![ent("x", json!({"Script":{"asset":"mesh"}}))])), "project.asset.kind");
    declared_scene_asset_with_unknown_texture_fails_load => (|p| p.add_scene("scene:bad", "bad.json", scene_json("bad", vec![ent("x", json!({"Transform":{"position":[0,0,0]},"SpriteRenderer":{"texture":"ghost"}}))])), "project.asset.unknown");
    declared_scene_asset_with_texture_as_mesh_fails_load => (|p| p.add_scene("scene:bad", "bad.json", scene_json("bad", vec![ent("x", json!({"MeshRenderer":{"mesh":"texture"}}))])), "project.asset.kind");
    declared_scene_asset_with_unsupported_schema_fails_load => (|p| p.add_scene("scene:bad", "bad.json", json!({"schema":2,"scene_id":"bad","entities":[]})), "project.schema.unsupported");
    declared_scene_asset_that_is_malformed_fails_load => (|p| { p.write("bad.json", "{{"); p.add_asset("scene:bad", "scene", "bad.json"); }, "project.document.malformed");
    startup_scene_unknown_script_asset_is_rejected => (|p| p.with_script("player", "ghost"), "project.asset.unknown");
    startup_scene_mesh_renderer_with_unknown_mesh_is_rejected => (|p| p.entity("player")["components"]["MeshRenderer"] = json!({"mesh":"ghost"}), "project.asset.unknown");
    startup_scene_mesh_renderer_with_texture_slot_of_wrong_kind_is_rejected => (|p| p.entity("player")["components"]["MeshRenderer"] = json!({"mesh":"mesh","texture":"mesh"}), "project.asset.kind");
}

#[test]
fn script_of_exactly_one_mebibyte_is_accepted() {
    let p = Proj::new();
    p.write("player.rs", vec![b'x'; 1024 * 1024]);
    assert!(p.load().is_ok());
}

#[test]
fn name_of_exactly_256_bytes_is_accepted() {
    let mut p = Proj::new();
    p.manifest["name"] = json!("n".repeat(256));
    assert_eq!(p.project().name.len(), 256);
}

#[test]
fn unicode_project_name_is_accepted_and_preserved() {
    let mut p = Proj::new();
    p.manifest["name"] = json!("Ωmega 十 🎮");
    assert_eq!(p.project().name, "Ωmega 十 🎮");
}

#[test]
fn asset_id_of_exactly_256_bytes_is_accepted() {
    let mut p = Proj::new();
    let id = "i".repeat(256);
    p.add_asset(&id, "script", "other2.rs");
    p.write("other2.rs", "x");
    assert!(p.project().assets.contains_key(&id));
}

#[test]
fn dot_segment_in_an_asset_path_is_accepted() {
    let mut p = Proj::new();
    p.write("scripts/extra.rs", "x");
    p.add_asset("extra", "script", "./scripts/./extra.rs");
    assert!(
        p.project()
            .resolve_asset("extra")
            .unwrap()
            .ends_with("extra.rs")
    );
}

#[test]
fn nested_directory_assets_resolve_inside_the_root() {
    let mut p = Proj::new();
    p.write("deep/er/still/x.rs", "x");
    p.add_asset("deep", "script", "deep/er/still/x.rs");
    let project = p.project();
    assert!(
        project
            .resolve_asset("deep")
            .unwrap()
            .starts_with(&project.root)
    );
}

#[test]
fn unlisted_startup_scene_file_is_valid_without_a_scene_asset() {
    let mut p = Proj::new();
    p.write(
        "start.json",
        serde_json::to_vec(&scene_json("start", vec![marker("a", 3.0, 4.0)])).unwrap(),
    );
    p.manifest["startup_scene"] = json!("start.json");
    let project = p.project();
    assert_eq!(project.scene.id, "start");
    assert_eq!(
        project.scene.entities[0]
            .components
            .transform
            .as_ref()
            .unwrap()
            .position,
        [3.0, 4.0, 0.0]
    );
}

#[test]
fn valid_extra_scene_asset_loads_alongside_the_startup_scene() {
    let mut p = Proj::new();
    p.add_scene(
        "scene:b",
        "b.json",
        scene_json("room:b", vec![wall("w", 5.0, 0.0, 1.0, 1.0)]),
    );
    assert!(p.load().is_ok());
}

#[test]
fn symlinked_asset_escaping_the_root_is_rejected() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("evil.rs"), "x").unwrap();
    let mut p = Proj::new();
    if !try_symlink_file(&outside.path().join("evil.rs"), &p.root().join("link.rs")) {
        eprintln!("symlink creation unavailable; escape test skipped");
        return;
    }
    p.add_asset("evil", "script", "link.rs");
    expect_code(p.load(), "project.path.outside_root");
}

#[test]
fn symlinked_directory_escaping_the_root_is_rejected() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("evil.rs"), "x").unwrap();
    let mut p = Proj::new();
    if !try_symlink_dir(outside.path(), &p.root().join("linkdir")) {
        eprintln!("symlink creation unavailable; escape test skipped");
        return;
    }
    p.add_asset("evil", "script", "linkdir/evil.rs");
    expect_code(p.load(), "project.path.outside_root");
}

#[test]
fn symlink_that_stays_inside_the_root_is_accepted() {
    let mut p = Proj::new();
    if !try_symlink_file(&p.root().join("other.rs"), &p.root().join("alias.rs")) {
        eprintln!("symlink creation unavailable; test skipped");
        return;
    }
    p.add_asset("alias", "script", "alias.rs");
    // Both ids canonicalize to one file, so the unique path rule fires.
    expect_code(p.load(), "project.asset.path.duplicate");
}

// ---- confined_file ---------------------------------------------------------

fn confined(rel: &str, expected: &str) {
    let p = Proj::new();
    p.write("sub/real.txt", "x");
    expect_code(schema::confined_file(p.root(), Path::new(rel)), expected);
}

table! { confined;
    confined_rejects_empty_path => ("", "project.path.outside_root");
    confined_rejects_parent_prefix => ("../x", "project.path.outside_root");
    confined_rejects_embedded_parent => ("sub/../player.rs", "project.path.outside_root");
    confined_rejects_bare_parent => ("..", "project.path.outside_root");
    confined_rejects_current_directory_as_non_file => (".", "project.path.outside_root");
    confined_rejects_directory_target => ("sub", "project.path.outside_root");
    confined_reports_missing_file => ("sub/none.txt", "project.asset.missing");
    confined_reports_missing_directory => ("nodir/none.txt", "project.asset.missing");
}

#[test]
fn confined_rejects_absolute_paths_even_inside_the_root() {
    let p = Proj::new();
    let absolute = p.root().join("player.rs");
    expect_code(
        schema::confined_file(p.root(), &absolute),
        "project.path.outside_root",
    );
}

#[test]
fn confined_returns_a_canonical_path_for_a_real_file() {
    let p = Proj::new();
    p.write("sub/real.txt", "x");
    let found = schema::confined_file(p.root(), Path::new("sub/real.txt")).unwrap();
    assert_eq!(
        found,
        fs::canonicalize(p.root().join("sub/real.txt")).unwrap()
    );
}

#[test]
fn confined_accepts_current_directory_segments() {
    let p = Proj::new();
    assert!(schema::confined_file(p.root(), Path::new("./player.rs")).is_ok());
}

#[test]
fn confined_error_diagnostic_names_the_relative_file() {
    let p = Proj::new();
    let error = schema::confined_file(p.root(), Path::new("../x")).unwrap_err();
    assert_eq!(error.diagnostics[0].file, PathBuf::from("../x"));
    assert_eq!(error.diagnostics[0].severity, "error");
}

// ---- input bindings in the manifest -----------------------------------------

fn bindings_of(p: &Proj) -> std::collections::BTreeMap<String, Vec<String>> {
    p.project().input_bindings
}

#[test]
fn bare_input_action_key_gets_the_key_prefix() {
    let p = Proj::new();
    assert_eq!(bindings_of(&p)["right"], vec!["Key:Right"]);
}

#[test]
fn prefixed_input_action_token_is_kept_verbatim() {
    let mut p = Proj::new();
    p.manifest["inputActions"] = json!({"fire": "Mouse:Left", "jump": "Gamepad:A"});
    let b = bindings_of(&p);
    assert_eq!(b["fire"], vec!["Mouse:Left"]);
    assert_eq!(b["jump"], vec!["Gamepad:A"]);
}

#[test]
fn explicit_bindings_take_precedence_over_input_actions() {
    let mut p = Proj::new();
    p.manifest["inputActions"] = json!({"jump": "Enter"});
    p.manifest["inputBindings"] = json!({"jump": ["Key:Space", "Gamepad:A"]});
    assert_eq!(bindings_of(&p)["jump"], vec!["Key:Space", "Gamepad:A"]);
}

#[test]
fn input_actions_map_is_retained_unchanged_next_to_bindings() {
    let mut p = Proj::new();
    p.manifest["inputActions"] = json!({"jump": "Enter"});
    p.manifest["inputBindings"] = json!({"jump": ["Key:Space"]});
    assert_eq!(p.project().input_actions["jump"], "Enter");
}

#[test]
fn actions_and_bindings_merge_into_one_map() {
    let mut p = Proj::new();
    p.manifest["inputActions"] = json!({"left": "Left"});
    p.manifest["inputBindings"] = json!({"right": ["Key:D"]});
    let b = bindings_of(&p);
    assert_eq!(b.len(), 2);
    assert_eq!(b["left"], vec!["Key:Left"]);
    assert_eq!(b["right"], vec!["Key:D"]);
}

#[test]
fn project_without_input_sections_has_no_bindings() {
    let mut p = Proj::new();
    p.manifest.as_object_mut().unwrap().remove("inputActions");
    assert!(bindings_of(&p).is_empty());
}

#[test]
fn conflicting_bindings_are_allowed_when_loading() {
    let mut p = Proj::new();
    p.manifest["inputBindings"] = json!({"a": ["Key:A"], "b": ["Key:A"]});
    assert_eq!(bindings_of(&p)["b"], vec!["Key:A"]);
}

#[test]
fn sixteen_bindings_for_one_action_are_accepted() {
    let mut p = Proj::new();
    let tokens: Vec<String> = ('A'..='P').map(|c| format!("Key:{c}")).collect();
    assert_eq!(tokens.len(), 16);
    p.manifest["inputBindings"] = json!({"many": tokens});
    assert_eq!(bindings_of(&p)["many"].len(), 16);
}

#[test]
fn exactly_256_actions_are_accepted() {
    let mut p = Proj::new();
    let mut all = serde_json::Map::new();
    for i in 0..256 {
        all.insert(format!("action{i}"), json!(["Key:A"]));
    }
    p.manifest["inputBindings"] = Value::Object(all);
    p.manifest.as_object_mut().unwrap().remove("inputActions");
    assert_eq!(bindings_of(&p).len(), 256);
}

table! { fails;
    input_unknown_bare_key_is_rejected => (|p| p.manifest["inputActions"] = json!({"x": "Nope"}), "project.input.invalid");
    input_lowercase_key_is_rejected => (|p| p.manifest["inputActions"] = json!({"x": "a"}), "project.input.invalid");
    input_unknown_token_in_bindings_is_rejected => (|p| p.manifest["inputBindings"] = json!({"x": ["Key:F26"]}), "project.input.invalid");
    input_unknown_mouse_button_is_rejected => (|p| p.manifest["inputBindings"] = json!({"x": ["Mouse:9"]}), "project.input.invalid");
    input_duplicate_token_within_one_action_is_rejected => (|p| p.manifest["inputBindings"] = json!({"x": ["Key:A", "Key:A"]}), "project.input.invalid");
    input_empty_binding_list_is_rejected => (|p| p.manifest["inputBindings"] = json!({"x": []}), "project.input.invalid");
    input_empty_action_name_is_rejected => (|p| p.manifest["inputBindings"] = json!({"": ["Key:A"]}), "project.input.invalid");
    input_seventeen_tokens_for_one_action_are_rejected => (|p| { let t: Vec<String> = ('A'..='Q').map(|c| format!("Key:{c}")).collect(); p.manifest["inputBindings"] = json!({"x": t}); }, "project.input.invalid");
    input_257_actions_are_rejected => (|p| { let mut all = serde_json::Map::new(); for i in 0..257 { all.insert(format!("a{i}"), json!(["Key:A"])); } p.manifest["inputBindings"] = Value::Object(all); }, "project.input.invalid");
    input_bindings_wrong_shape_is_malformed => (|p| p.manifest["inputBindings"] = json!({"x": "Key:A"}), "project.document.malformed");
    input_action_value_of_wrong_type_is_malformed => (|p| p.manifest["inputActions"] = json!({"x": 5}), "project.document.malformed");
}

// ---- external Rust crate selection -------------------------------------------

#[test]
fn default_rust_manifest_path_is_cargo_toml_without_requiring_it() {
    let p = Proj::new();
    let project = p.project();
    assert_eq!(project.rust_manifest, PathBuf::from("Cargo.toml"));
}

#[test]
fn default_rust_manifest_resolution_fails_when_the_file_is_absent() {
    let p = Proj::new();
    expect_code(p.project().rust_manifest(), "project.asset.missing");
}

#[test]
fn default_rust_manifest_resolves_when_adjacent_file_exists() {
    let p = Proj::new();
    p.write("Cargo.toml", "[package]\n");
    let resolved = p.project().rust_manifest().unwrap();
    assert!(resolved.is_file() && resolved.ends_with("Cargo.toml"));
}

#[test]
fn explicit_rust_manifest_selects_a_nested_crate() {
    let mut p = Proj::new();
    p.write("gameplay/Cargo.toml", "[package]\n");
    p.manifest["rust"] = json!({"manifest": "gameplay/Cargo.toml"});
    let project = p.project();
    assert_eq!(project.rust_manifest, PathBuf::from("gameplay/Cargo.toml"));
    assert!(project.rust_manifest().unwrap().ends_with("Cargo.toml"));
}

table! { fails;
    explicit_rust_manifest_must_exist => (|p| p.manifest["rust"] = json!({"manifest": "missing/Cargo.toml"}), "project.asset.missing");
    explicit_rust_manifest_cannot_traverse_upwards => (|p| p.manifest["rust"] = json!({"manifest": "../Cargo.toml"}), "project.path.outside_root");
    explicit_rust_manifest_unknown_field_is_rejected => (|p| p.manifest["rust"] = json!({"manifest": "Cargo.toml", "x": 1}), "project.document.malformed");
    explicit_rust_manifest_requires_manifest_field => (|p| p.manifest["rust"] = json!({}), "project.document.malformed");
    explicit_rust_manifest_cannot_be_empty => (|p| p.manifest["rust"] = json!({"manifest": ""}), "project.path.outside_root");
}

#[test]
fn explicit_absolute_rust_manifest_is_refused() {
    let mut p = Proj::new();
    p.write("Cargo.toml", "x");
    let absolute = p.root().join("Cargo.toml");
    p.manifest["rust"] = json!({"manifest": absolute});
    expect_code(p.load(), "project.path.outside_root");
}

// ---- asset accessors --------------------------------------------------------

#[test]
fn resolve_asset_returns_the_canonical_file() {
    let p = Proj::new();
    let project = p.project();
    let file = project.resolve_asset("mesh").unwrap();
    assert_eq!(file, fs::canonicalize(p.root().join("quad.mesh")).unwrap());
}

#[test]
fn resolve_unknown_asset_reports_its_id() {
    let p = Proj::new();
    let error = p.project().resolve_asset("ghost").unwrap_err();
    assert_eq!(error.diagnostics[0].code, "project.asset.unknown");
    assert!(error.diagnostics[0].message.contains("ghost"));
}

#[test]
fn resolve_asset_reports_a_file_deleted_after_load() {
    let p = Proj::new();
    let project = p.project();
    fs::remove_file(p.root().join("quad.mesh")).unwrap();
    expect_code(project.resolve_asset("mesh"), "project.asset.missing");
}

#[test]
fn require_asset_returns_the_matching_declaration() {
    let p = Proj::new();
    let project = p.project();
    let asset = project.require_asset("font", "font").unwrap();
    assert_eq!(asset.path, PathBuf::from("font.json"));
}

#[test]
fn require_asset_rejects_a_different_kind_and_names_the_asset() {
    let p = Proj::new();
    let project = p.project();
    let error = project.require_asset("font", "texture").unwrap_err();
    assert_eq!(error.diagnostics[0].code, "project.asset.kind");
    assert!(error.diagnostics[0].message.contains("font"));
}

#[test]
fn require_asset_rejects_unknown_ids() {
    let p = Proj::new();
    expect_code(
        p.project().require_asset("ghost", "texture"),
        "project.asset.unknown",
    );
}

#[test]
fn validate_scene_rejects_a_foreign_scene_with_unknown_script() {
    let p = Proj::new();
    let project = p.project();
    let mut scene = project.scene.clone();
    scene.entities[0].components.script = Some(engine_project::Script {
        version: 1,
        asset: "ghost".into(),
    });
    expect_code(project.validate_scene(&scene), "project.asset.unknown");
}

#[test]
fn project_validate_notices_a_texture_broken_after_load() {
    let p = Proj::new();
    let project = p.project();
    p.write("sheet.ppm", "P3 1 1 255 0 0");
    expect_code(project.validate(), "asset.texture.invalid");
}

#[test]
fn diagnostics_render_code_and_message_in_display() {
    let p = Proj::new();
    let error = p.project().resolve_asset("ghost").unwrap_err();
    assert_eq!(
        error.to_string(),
        "project.asset.unknown: Unknown asset ghost"
    );
}

#[test]
fn diagnostics_with_location_serialize_to_json() {
    let p = Proj::new();
    let error = schema::confined_file(p.root(), Path::new("../x")).unwrap_err();
    let value = serde_json::to_value(&error).unwrap();
    assert_eq!(value["diagnostics"][0]["code"], "project.path.outside_root");
    assert_eq!(value["diagnostics"][0]["file"], "../x");
}
