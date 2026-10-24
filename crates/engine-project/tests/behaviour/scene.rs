//! Scene manifests: component schema, defaults, validation bounds, stable IDs and
//! revision hashing.
use super::common::*;
use engine_project::{Camera2D, Collider2D, Scene, SpriteRenderer, Transform, schema};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Mutate the player's components and expect `load` to fail with `expected`.
fn bad_component(mutate: impl FnOnce(&mut Value), expected: &str) {
    let mut p = Proj::new();
    mutate(&mut p.entity("player")["components"]);
    expect_code(p.load(), expected);
}

/// Mutate the player's components and expect `load` to succeed.
fn good_component(mutate: impl FnOnce(&mut Value)) {
    let mut p = Proj::new();
    mutate(&mut p.entity("player")["components"]);
    p.load().unwrap();
}

fn bad_scene(mutate: impl FnOnce(&mut Proj), expected: &str) {
    let mut p = Proj::new();
    mutate(&mut p);
    expect_code(p.load(), expected);
}

fn bad_struct(mutate: impl FnOnce(&mut Scene), expected: &str) {
    let p = Proj::new();
    let mut scene = p.project().scene;
    mutate(&mut scene);
    expect_code(scene.validate(), expected);
}

table! { bad_component;
    transform_version_two_is_rejected => (|c| c["Transform"]["version"] = json!(2), "scene.component.invalid");
    transform_version_zero_is_rejected => (|c| c["Transform"]["version"] = json!(0), "scene.component.invalid");
    transform_with_two_components_is_malformed => (|c| c["Transform"]["position"] = json!([0, 0]), "project.document.malformed");
    transform_with_four_components_is_malformed => (|c| c["Transform"]["position"] = json!([0, 0, 0, 0]), "project.document.malformed");
    transform_position_string_is_malformed => (|c| c["Transform"]["position"] = json!("0,0,0"), "project.document.malformed");
    transform_unknown_field_is_malformed => (|c| c["Transform"]["scale"] = json!(2), "project.document.malformed");
    transform_missing_position_is_malformed => (|c| { c["Transform"].as_object_mut().unwrap().remove("position"); }, "project.document.malformed");
    script_version_two_is_rejected => (|c| c["Script"] = json!({"version": 2, "asset": "player"}), "scene.component.invalid");
    script_empty_asset_is_rejected => (|c| c["Script"] = json!({"asset": ""}), "scene.component.invalid");
    script_control_character_asset_is_rejected => (|c| c["Script"] = json!({"asset": "pl\nayer"}), "scene.component.invalid");
    script_overlong_asset_is_rejected => (|c| c["Script"] = json!({"asset": "p".repeat(257)}), "scene.component.invalid");
    mesh_renderer_version_two_is_rejected => (|c| c["MeshRenderer"] = json!({"version": 2, "mesh": "mesh"}), "scene.component.invalid");
    mesh_renderer_empty_mesh_is_rejected => (|c| c["MeshRenderer"] = json!({"mesh": ""}), "scene.component.invalid");
    mesh_renderer_empty_texture_is_rejected => (|c| c["MeshRenderer"] = json!({"mesh": "mesh", "texture": ""}), "scene.component.invalid");
    sprite_requires_transform => (|c| { c.as_object_mut().unwrap().remove("Transform"); c.as_object_mut().unwrap().remove("Collider2D"); }, "scene.component.invalid");
    collider_requires_transform => (|c| { c.as_object_mut().unwrap().remove("Transform"); c.as_object_mut().unwrap().remove("SpriteRenderer"); }, "scene.component.invalid");
    sprite_version_two_is_rejected => (|c| c["SpriteRenderer"]["version"] = json!(2), "scene.component.invalid");
    sprite_empty_texture_is_rejected => (|c| c["SpriteRenderer"]["texture"] = json!(""), "scene.component.invalid");
    sprite_zero_width_is_rejected => (|c| c["SpriteRenderer"]["size"] = json!([0, 1]), "scene.component.invalid");
    sprite_zero_height_is_rejected => (|c| c["SpriteRenderer"]["size"] = json!([1, 0]), "scene.component.invalid");
    sprite_negative_size_is_rejected => (|c| c["SpriteRenderer"]["size"] = json!([-1, 1]), "scene.component.invalid");
    sprite_pivot_above_one_is_rejected => (|c| c["SpriteRenderer"]["pivot"] = json!([1.5, 0.5]), "scene.component.invalid");
    sprite_pivot_below_zero_is_rejected => (|c| c["SpriteRenderer"]["pivot"] = json!([0.5, -0.1]), "scene.component.invalid");
    sprite_tint_above_one_is_rejected => (|c| c["SpriteRenderer"]["tint"] = json!([1, 1, 1, 1.1]), "scene.component.invalid");
    sprite_tint_below_zero_is_rejected => (|c| c["SpriteRenderer"]["tint"] = json!([-0.1, 1, 1, 1]), "scene.component.invalid");
    sprite_zero_width_source_rect_is_rejected => (|c| c["SpriteRenderer"]["source"] = json!([0, 0, 0, 1]), "scene.component.invalid");
    sprite_source_rect_beyond_8192_is_rejected => (|c| c["SpriteRenderer"]["source"] = json!([8190, 0, 4, 1]), "scene.component.invalid");
    sprite_source_rect_overflowing_u32_is_rejected => (|c| c["SpriteRenderer"]["source"] = json!([4294967295u32, 0, 1, 1]), "scene.component.invalid");
    sprite_zero_height_frame_is_rejected => (|c| c["SpriteRenderer"]["frames"] = json!([[0, 0, 1, 1], [1, 0, 1, 0]]), "scene.component.invalid");
    sprite_frame_beyond_8192_is_rejected => (|c| c["SpriteRenderer"]["frames"] = json!([[0, 8190, 1, 3]]), "scene.component.invalid");
    sprite_negative_fps_is_rejected => (|c| c["SpriteRenderer"]["framesPerSecond"] = json!(-1), "scene.component.invalid");
    sprite_fps_above_1000_is_rejected => (|c| c["SpriteRenderer"]["framesPerSecond"] = json!(1000.5), "scene.component.invalid");
    sprite_more_than_4096_frames_is_rejected => (|c| c["SpriteRenderer"]["frames"] = json!(vec![[0, 0, 1, 1]; 4097]), "scene.component.invalid");
    sprite_unknown_field_is_malformed => (|c| c["SpriteRenderer"]["spin"] = json!(1), "project.document.malformed");
    sprite_snake_case_fps_key_is_malformed => (|c| c["SpriteRenderer"]["frames_per_second"] = json!(4), "project.document.malformed");
    collider_version_two_is_rejected => (|c| c["Collider2D"]["version"] = json!(2), "scene.component.invalid");
    collider_zero_size_is_rejected => (|c| c["Collider2D"]["size"] = json!([0, 1]), "scene.component.invalid");
    collider_negative_size_is_rejected => (|c| c["Collider2D"]["size"] = json!([1, -2]), "scene.component.invalid");
    collider_huge_size_is_rejected => (|c| c["Collider2D"]["size"] = json!([1000001, 1]), "scene.component.invalid");
    collider_huge_offset_is_rejected => (|c| c["Collider2D"]["offset"] = json!([0, -1000001]), "scene.component.invalid");
    collider_zero_layer_is_rejected => (|c| c["Collider2D"]["layer"] = json!(0), "scene.component.invalid");
    collider_zero_mask_is_rejected => (|c| c["Collider2D"]["mask"] = json!(0), "scene.component.invalid");
    collider_layer_beyond_u32_is_malformed => (|c| c["Collider2D"]["layer"] = json!(4294967296u64), "project.document.malformed");
    collider_unknown_field_is_malformed => (|c| c["Collider2D"]["friction"] = json!(1), "project.document.malformed");
    components_unknown_member_is_malformed => (|c| c["Rigidbody"] = json!({}), "project.document.malformed");
}

table! { good_component;
    sprite_pivot_corners_are_inclusive => (|c| c["SpriteRenderer"]["pivot"] = json!([0, 1]));
    sprite_tint_extremes_are_inclusive => (|c| c["SpriteRenderer"]["tint"] = json!([0, 0, 0, 0]));
    sprite_fps_1000_is_inclusive => (|c| c["SpriteRenderer"]["framesPerSecond"] = json!(1000));
    sprite_source_rect_touching_8192_is_inclusive => (|c| c["SpriteRenderer"]["source"] = json!([8191, 8191, 1, 1]));
    sprite_4096_frames_are_inclusive => (|c| c["SpriteRenderer"]["frames"] = json!(vec![[0, 0, 1, 1]; 4096]));
    collider_size_1e6_is_inclusive => (|c| c["Collider2D"]["size"] = json!([1000000, 1000000]));
    collider_offset_1e6_is_inclusive => (|c| c["Collider2D"]["offset"] = json!([1000000, -1000000]));
    collider_highest_layer_bit_is_valid => (|c| c["Collider2D"]["layer"] = json!(2147483648u32));
    collider_trigger_flag_is_valid => (|c| c["Collider2D"]["trigger"] = json!(true));
    sprite_hidden_flag_is_valid => (|c| c["SpriteRenderer"]["visible"] = json!(false));
    sprite_negative_layer_is_valid => (|c| c["SpriteRenderer"]["layer"] = json!(-12));
    mesh_renderer_with_registered_texture_is_valid => (|c| c["MeshRenderer"] = json!({"mesh": "mesh", "texture": "texture"}));
    script_with_registered_asset_is_valid => (|c| c["Script"] = json!({"asset": "player"}));
    transform_with_extreme_finite_position_is_valid => (|c| c["Transform"]["position"] = json!([3.0e38, -3.0e38, 0]));
}

table! { bad_scene;
    scene_schema_zero_is_unsupported => (|p| p.scene["schema"] = json!(0), "project.schema.unsupported");
    scene_schema_two_is_unsupported => (|p| p.scene["schema"] = json!(2), "project.schema.unsupported");
    scene_empty_id_is_invalid => (|p| p.scene["scene_id"] = json!(""), "scene.invalid");
    scene_overlong_id_is_invalid => (|p| p.scene["scene_id"] = json!("s".repeat(257)), "scene.invalid");
    scene_control_character_id_is_invalid => (|p| p.scene["scene_id"] = json!("a\u{0}b"), "scene.invalid");
    scene_unknown_field_is_malformed => (|p| p.scene["extra"] = json!(1), "project.document.malformed");
    scene_missing_entities_is_malformed => (|p| { p.scene.as_object_mut().unwrap().remove("entities"); }, "project.document.malformed");
    scene_missing_id_is_malformed => (|p| { p.scene.as_object_mut().unwrap().remove("scene_id"); }, "project.document.malformed");
    scene_alias_id_key_is_malformed => (|p| { p.scene["id"] = json!("x"); }, "project.document.malformed");
    entity_duplicate_id_is_rejected => (|p| p.add_entity(marker("player", 9.0, 9.0)), "scene.entity.id.duplicate");
    entity_empty_id_is_rejected => (|p| p.add_entity(marker("", 0.0, 0.0)), "scene.entity.id.duplicate");
    entity_empty_name_is_rejected => (|p| p.add_entity(json!({"id":"n","name":"","components":{}})), "scene.entity.id.duplicate");
    entity_overlong_id_is_rejected => (|p| p.add_entity(marker(&"e".repeat(257), 0.0, 0.0)), "scene.entity.id.duplicate");
    entity_control_character_name_is_rejected => (|p| p.add_entity(json!({"id":"n","name":"a\nb","components":{}})), "scene.entity.id.duplicate");
    entity_missing_name_is_malformed => (|p| p.add_entity(json!({"id":"n","components":{}})), "project.document.malformed");
    entity_missing_components_is_malformed => (|p| p.add_entity(json!({"id":"n","name":"n"})), "project.document.malformed");
    entity_unknown_field_is_malformed => (|p| p.add_entity(json!({"id":"n","name":"n","components":{},"tag":"x"})), "project.document.malformed");
    camera_second_instance_is_rejected => (|p| p.add_entity(ent("cam2", json!({"Camera2D":{}}))), "scene.camera.multiple");
    camera_follow_unknown_entity_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["follow"] = json!("ghost"), "scene.camera.follow");
    camera_follow_entity_without_transform_is_rejected => (|p| { p.add_entity(ent("bare", json!({}))); p.entity("camera")["components"]["Camera2D"]["follow"] = json!("bare"); }, "scene.camera.follow");
    camera_version_two_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["version"] = json!(2), "scene.component.invalid");
    camera_zero_logical_width_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["logicalSize"] = json!([0, 180]), "scene.component.invalid");
    camera_zero_logical_height_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["logicalSize"] = json!([320, 0]), "scene.component.invalid");
    camera_logical_size_above_8192_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["logicalSize"] = json!([8193, 10]), "scene.component.invalid");
    camera_zero_pixels_per_unit_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["pixelsPerUnit"] = json!(0), "scene.component.invalid");
    camera_negative_pixels_per_unit_is_rejected => (|p| p.entity("camera")["components"]["Camera2D"]["pixelsPerUnit"] = json!(-4), "scene.component.invalid");
    camera_unknown_field_is_malformed => (|p| p.entity("camera")["components"]["Camera2D"]["zoom"] = json!(2), "project.document.malformed");
    scene_with_65537_entities_exceeds_capacity => (|p| { let many: Vec<Value> = (0..65537).map(|i| ent(&format!("e{i}"), json!({}))).collect(); p.scene["entities"] = Value::Array(many); }, "scene.invalid");
}

#[test]
fn scene_with_65536_entities_is_within_capacity() {
    let mut p = Proj::new();
    let many: Vec<Value> = (0..65536)
        .map(|i| ent(&format!("e{i}"), json!({})))
        .collect();
    p.scene["entities"] = Value::Array(many);
    assert_eq!(p.project().scene.entities.len(), 65536);
}

#[test]
fn camera_logical_size_8192_is_inclusive() {
    let mut p = Proj::new();
    p.entity("camera")["components"]["Camera2D"]["logicalSize"] = json!([8192, 8192]);
    assert_eq!(
        p.project().scene.entities[1]
            .components
            .camera_2d
            .as_ref()
            .unwrap()
            .logical_size,
        [8192, 8192]
    );
}

#[test]
fn camera_follow_a_transformed_entity_is_accepted() {
    let mut p = Proj::new();
    p.add_entity(marker("target", 4.0, 4.0));
    p.entity("camera")["components"]["Camera2D"]["follow"] = json!("target");
    assert!(p.load().is_ok());
}

#[test]
fn scene_without_a_camera_is_valid() {
    let mut p = Proj::new();
    p.scene["entities"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["id"] != "camera");
    assert_eq!(p.project().scene.entities.len(), 1);
}

#[test]
fn empty_scene_is_valid() {
    let mut p = Proj::new();
    p.scene["entities"] = json!([]);
    assert!(p.project().scene.entities.is_empty());
}

#[test]
fn entity_ids_may_contain_unicode_and_punctuation() {
    let mut p = Proj::new();
    p.add_entity(marker("door/Ω 十:1", 0.0, 0.0));
    assert!(
        p.project()
            .scene
            .entities
            .iter()
            .any(|e| e.id == "door/Ω 十:1")
    );
}

#[test]
fn entity_id_of_exactly_256_bytes_is_valid() {
    let mut p = Proj::new();
    p.add_entity(marker(&"e".repeat(256), 0.0, 0.0));
    assert!(p.load().is_ok());
}

#[test]
fn duplicate_entity_names_with_distinct_ids_are_valid() {
    let mut p = Proj::new();
    p.add_entity(json!({"id":"a","name":"Same","components":{}}));
    p.add_entity(json!({"id":"b","name":"Same","components":{}}));
    assert!(p.load().is_ok());
}

#[test]
fn entity_ids_are_case_sensitive() {
    let mut p = Proj::new();
    p.add_entity(marker("Player", 0.0, 0.0));
    assert!(p.load().is_ok());
}

#[test]
fn duplicate_entity_error_points_at_the_scene_file_and_id() {
    let mut p = Proj::new();
    p.add_entity(marker("player", 9.0, 9.0));
    let error = p.load().unwrap_err();
    assert_eq!(error.diagnostics[0].file, PathBuf::from("scene.json"));
    assert_eq!(error.diagnostics[0].path, "player");
}

#[test]
fn component_error_points_at_the_offending_entity() {
    let mut p = Proj::new();
    p.entity("player")["components"]["Collider2D"]["layer"] = json!(0);
    let error = p.load().unwrap_err();
    assert_eq!(error.diagnostics[0].code, "scene.component.invalid");
    assert_eq!(error.diagnostics[0].path, "player");
    assert_eq!(error.diagnostics[0].file, PathBuf::from("scene.json"));
}

// ---- struct-level validation of values JSON cannot express --------------------

table! { bad_struct;
    transform_nan_position_is_rejected => (|s| s.entities[0].components.transform.as_mut().unwrap().position[0] = f32::NAN, "scene.component.invalid");
    transform_infinite_position_is_rejected => (|s| s.entities[0].components.transform.as_mut().unwrap().position[2] = f32::INFINITY, "scene.component.invalid");
    sprite_nan_size_is_rejected => (|s| s.entities[0].components.sprite_renderer.as_mut().unwrap().size[1] = f32::NAN, "scene.component.invalid");
    sprite_infinite_pivot_is_rejected => (|s| s.entities[0].components.sprite_renderer.as_mut().unwrap().pivot[0] = f32::INFINITY, "scene.component.invalid");
    sprite_nan_tint_is_rejected => (|s| s.entities[0].components.sprite_renderer.as_mut().unwrap().tint[3] = f32::NAN, "scene.component.invalid");
    sprite_nan_fps_is_rejected => (|s| s.entities[0].components.sprite_renderer.as_mut().unwrap().frames_per_second = f32::NAN, "scene.component.invalid");
    collider_nan_size_is_rejected => (|s| s.entities[0].components.collider_2d.as_mut().unwrap().size[0] = f32::NAN, "scene.component.invalid");
    collider_infinite_offset_is_rejected => (|s| s.entities[0].components.collider_2d.as_mut().unwrap().offset[1] = f32::NEG_INFINITY, "scene.component.invalid");
    camera_nan_center_is_rejected => (|s| s.entities[1].components.camera_2d.as_mut().unwrap().center[0] = f32::NAN, "scene.component.invalid");
    camera_nan_pixels_per_unit_is_rejected => (|s| s.entities[1].components.camera_2d.as_mut().unwrap().pixels_per_unit = f32::NAN, "scene.component.invalid");
    camera_infinite_pixels_per_unit_is_rejected => (|s| s.entities[1].components.camera_2d.as_mut().unwrap().pixels_per_unit = f32::INFINITY, "scene.component.invalid");
    scene_schema_mutated_after_load_is_rejected => (|s| s.schema = 7, "project.schema.unsupported");
}

// ---- defaults and round trips ---------------------------------------------------

#[test]
fn sprite_defaults_match_the_authored_format() {
    let sprite: SpriteRenderer = serde_json::from_value(json!({"texture": "t"})).unwrap();
    assert_eq!(
        sprite,
        SpriteRenderer {
            texture: "t".into(),
            ..Default::default()
        }
    );
    assert_eq!(sprite.size, [1.0, 1.0]);
    assert_eq!(sprite.pivot, [0.5, 0.5]);
    assert_eq!(sprite.tint, [1.0; 4]);
    assert!(sprite.looping && sprite.visible);
    assert_eq!((sprite.layer, sprite.frames_per_second), (0, 0.0));
    assert!(sprite.source.is_none() && sprite.frames.is_empty());
}

#[test]
fn sprite_loop_flag_serializes_under_the_loop_key() {
    let sprite = SpriteRenderer {
        texture: "t".into(),
        looping: false,
        ..Default::default()
    };
    let value = serde_json::to_value(sprite).unwrap();
    assert_eq!(value["loop"], false);
    assert!(value.get("looping").is_none());
}

#[test]
fn camera_defaults_match_the_authored_format() {
    let camera: Camera2D = serde_json::from_value(json!({})).unwrap();
    assert_eq!(camera, Camera2D::default());
    assert_eq!(camera.logical_size, [320, 180]);
    assert_eq!(camera.pixels_per_unit, 16.0);
    assert!(camera.pixel_snap && camera.follow.is_none());
    assert_eq!(camera.center, [0.0, 0.0]);
}

#[test]
fn camera_fields_serialize_in_camel_case() {
    let value = serde_json::to_value(Camera2D::default()).unwrap();
    assert!(value.get("logicalSize").is_some());
    assert!(value.get("pixelsPerUnit").is_some());
    assert!(value.get("pixelSnap").is_some());
    assert!(value.get("follow").is_none());
}

#[test]
fn collider_defaults_cover_every_layer_and_are_solid() {
    let collider: Collider2D = serde_json::from_value(json!({"size": [2, 3]})).unwrap();
    assert_eq!(collider.layer, 1);
    assert_eq!(collider.mask, u32::MAX);
    assert!(!collider.trigger);
    assert_eq!(collider.offset, [0.0, 0.0]);
    assert_eq!(collider.version, 1);
}

#[test]
fn transform_version_defaults_to_one() {
    let transform: Transform = serde_json::from_value(json!({"position": [1, 2, 3]})).unwrap();
    assert_eq!(
        transform,
        Transform {
            version: 1,
            position: [1.0, 2.0, 3.0],
            ..Default::default()
        }
    );
    assert_eq!(Transform::default().position, [0.0; 3]);
}

#[test]
fn scene_round_trips_through_json_unchanged() {
    let p = Proj::new();
    let scene = p.project().scene;
    let text = serde_json::to_string(&scene).unwrap();
    let mut again: Scene = serde_json::from_str(&text).unwrap();
    again.source = scene.source.clone();
    assert_eq!(again, scene);
}

#[test]
fn scene_serialization_omits_the_source_path() {
    let p = Proj::new();
    let value = serde_json::to_value(p.project().scene).unwrap();
    assert!(value.get("source").is_none());
    assert_eq!(value["scene_id"], "room:a");
}

#[test]
fn absent_components_are_not_serialized() {
    let p = Proj::new();
    let value = serde_json::to_value(p.project().scene).unwrap();
    let camera = &value["entities"][1]["components"];
    assert!(camera.get("Transform").is_none() && camera.get("Script").is_none());
    assert!(camera.get("Camera2D").is_some());
}

#[test]
fn explicit_components_override_defaults_when_loaded() {
    let mut p = Proj::new();
    p.entity("player")["components"]["SpriteRenderer"] = json!({"texture":"texture","size":[2,3],"pivot":[0,1],"tint":[0.5,0.25,0,1],"layer":4,"loop":false,"visible":false,"framesPerSecond":12});
    let project = p.project();
    let s = project.scene.entities[0]
        .components
        .sprite_renderer
        .as_ref()
        .unwrap();
    assert_eq!((s.size, s.pivot, s.layer), ([2.0, 3.0], [0.0, 1.0], 4));
    assert_eq!(s.tint, [0.5, 0.25, 0.0, 1.0]);
    assert!(!s.looping && !s.visible);
    assert_eq!(s.frames_per_second, 12.0);
}

#[test]
fn scene_source_is_the_manifest_relative_path() {
    let mut p = Proj::new();
    p.add_scene("scene:b", "rooms/b.json", scene_json("room:b", vec![]));
    let project = p.project();
    let scene = schema::load_scene(&project.root, Path::new("rooms/b.json")).unwrap();
    assert_eq!(scene.source, PathBuf::from("rooms/b.json"));
}

#[test]
fn load_scene_refuses_traversal_outside_the_root() {
    let p = Proj::new();
    expect_code(
        schema::load_scene(p.root(), Path::new("../scene.json")),
        "project.path.outside_root",
    );
}

#[test]
fn load_scene_refuses_oversized_files() {
    let p = Proj::new();
    p.write("huge.json", vec![b' '; 16 * 1024 * 1024 + 1]);
    expect_code(
        schema::load_scene(p.root(), Path::new("huge.json")),
        "project.file.too_large",
    );
}

#[test]
fn load_scene_rejects_a_scene_that_is_valid_json_but_invalid_schema() {
    let p = Proj::new();
    p.write(
        "odd.json",
        r#"{"schema":1,"scene_id":"x","entities":[{"id":"a"}]}"#,
    );
    expect_code(
        schema::load_scene(p.root(), Path::new("odd.json")),
        "project.document.malformed",
    );
}

// ---- revision hashing -----------------------------------------------------------

#[test]
fn revision_is_a_64_character_hex_digest() {
    let p = Proj::new();
    let revision = p.project().scene.revision();
    assert_eq!(revision.len(), 64);
    assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn revision_is_stable_for_identical_content() {
    let p = Proj::new();
    assert_eq!(p.project().scene.revision(), p.project().scene.revision());
}

#[test]
fn revision_changes_when_a_position_changes() {
    let p = Proj::new();
    let mut scene = p.project().scene;
    let before = scene.revision();
    scene.entities[0]
        .components
        .transform
        .as_mut()
        .unwrap()
        .position[0] = 1.0;
    assert_ne!(scene.revision(), before);
}

#[test]
fn revision_changes_when_entity_order_changes() {
    let p = Proj::new();
    let mut scene = p.project().scene;
    let before = scene.revision();
    scene.entities.swap(0, 1);
    assert_ne!(scene.revision(), before);
}

#[test]
fn revision_ignores_the_runtime_only_source_path() {
    let p = Proj::new();
    let mut scene = p.project().scene;
    let before = scene.revision();
    scene.source = PathBuf::from("elsewhere.json");
    assert_eq!(scene.revision(), before);
}

#[test]
fn revision_returns_after_reverting_a_change() {
    let p = Proj::new();
    let mut scene = p.project().scene;
    let before = scene.revision();
    scene.entities[0]
        .components
        .transform
        .as_mut()
        .unwrap()
        .position[1] = 5.0;
    scene.entities[0]
        .components
        .transform
        .as_mut()
        .unwrap()
        .position[1] = 0.0;
    assert_eq!(scene.revision(), before);
}

#[test]
fn revision_changes_with_the_scene_id() {
    let p = Proj::new();
    let mut scene = p.project().scene;
    let before = scene.revision();
    scene.id = "room:renamed".into();
    assert_ne!(scene.revision(), before);
}
