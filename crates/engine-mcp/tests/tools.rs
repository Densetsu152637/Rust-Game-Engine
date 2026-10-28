//! Success and failure paths of every read-only tool, plus confinement.

mod common;

use common::*;
use serde_json::{Value, json};
use std::fs;

// ---- project_info ----------------------------------------------------------------------

#[test]
fn project_info_summarises_the_manifest() {
    let dir = fixture();
    let info = data(dir.path(), "project_info", json!({}));
    assert_eq!(info["ok"], true);
    assert_eq!(info["name"], "Tool Fixture");
    assert_eq!(info["schema"], 1);
    assert_eq!(info["manifest"], "project.json");
    assert_eq!(info["startup_scene"], "scenes/start.json");
    assert_eq!(info["assets"]["total"], 4);
    assert_eq!(
        info["assets"]["by_kind"],
        json!({"mesh":1,"scene":1,"script":1,"texture":1})
    );
    assert_eq!(
        info["scripts"],
        json!([{"asset_id":"asset:script","path":"src/lib.rs"}])
    );
    assert_eq!(info["rust"]["package_name"], "tool-fixture");
    assert_eq!(info["rust"]["present"], true);
    let scenes = info["scenes"].as_array().unwrap();
    assert_eq!(scenes.len(), 2);
    assert_eq!(scenes[0]["startup"], true);
    assert_eq!(scenes[1]["asset_id"], "asset:second");
    assert_eq!(info["settings"]["startup_camera"]["pixels_per_unit"], 16.0);
    let actions: Vec<_> = info["input_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["jump", "move_left"]);
}

#[test]
fn project_info_reports_the_access_level() {
    let dir = fixture();
    let read = data(dir.path(), "project_info", json!({}));
    assert_eq!(
        read["access"],
        json!({"read":true,"write":false,"exec":false})
    );
    let writable = write(dir.path(), "project_info", json!({}));
    assert_eq!(
        writable["access"],
        json!({"read":true,"write":true,"exec":false})
    );
}

#[test]
fn project_info_without_a_manifest_is_a_structured_failure() {
    let dir = tempfile::tempdir().unwrap();
    let info = data(dir.path(), "project_info", json!({}));
    assert_eq!(error_code(&info), "project_missing");
    assert!(
        info["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("project.json")
    );
}

#[test]
fn project_info_on_a_broken_project_returns_diagnostics() {
    let dir = fixture();
    fs::remove_file(dir.path().join("assets/white.ppm")).unwrap();
    let info = data(dir.path(), "project_info", json!({}));
    assert_eq!(info["ok"], false);
    assert!(!info["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn project_info_rejects_arguments() {
    let dir = fixture();
    let responses = exchange(
        dir.path(),
        read_only(),
        &[modern(
            1,
            "tools/call",
            json!({"name":"project_info","arguments":{"x":1}}),
        )],
    );
    assert_eq!(responses[0]["error"]["code"], -32602);
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not accept argument `x`")
    );
}

// ---- project_validate / input_actions ---------------------------------------------------

#[test]
fn project_validate_counts_scenes_and_assets() {
    let dir = fixture();
    let result = data(dir.path(), "project_validate", json!({}));
    assert_eq!(result["ok"], true);
    assert_eq!(result["scenes"], 2);
    assert_eq!(result["assets"], 4);
}

#[test]
fn input_actions_merge_legacy_keys_and_bindings() {
    let dir = fixture();
    let result = data(dir.path(), "input_actions", json!({}));
    assert_eq!(
        result["actions"],
        json!([
            {"name":"jump","bindings":["Key:Space"],"legacy_key":"Space"},
            {"name":"move_left","bindings":["Key:A","Key:Left"]}
        ])
    );
    assert!(result.get("binding_tokens").is_none());
}

#[test]
fn input_actions_can_list_the_valid_binding_tokens() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "input_actions",
        json!({"include_binding_tokens": true}),
    );
    let tokens = result["binding_tokens"].as_array().unwrap();
    assert!(tokens.contains(&json!("Key:Space")));
    assert!(tokens.contains(&json!("Gamepad:A")));
}

#[test]
fn input_actions_validates_argument_types() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "input_actions",
        json!({"include_binding_tokens": "yes"}),
    );
    assert_eq!(error_code(&result), "invalid_argument");
}

// ---- scene_list / scene_inspect / entity_get ---------------------------------------------

#[test]
fn scene_list_covers_startup_and_catalog_scenes() {
    let dir = fixture();
    let result = data(dir.path(), "scene_list", json!({}));
    let scenes = result["scenes"].as_array().unwrap();
    assert_eq!(scenes.len(), 2);
    assert_eq!(scenes[0]["path"], "scenes/start.json");
    assert_eq!(scenes[0]["scene_id"], "scene:start");
    assert_eq!(scenes[0]["entities"], 3);
    assert_eq!(scenes[0]["startup"], true);
    assert_eq!(scenes[0]["valid"], true);
    assert_eq!(scenes[0]["revision"].as_str().unwrap().len(), 64);
    assert_eq!(scenes[1]["path"], "scenes/second.json");
    assert_eq!(scenes[1]["asset_id"], "asset:second");
}

#[test]
fn scene_inspect_returns_scene_revision_and_clean_numbers() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/start.json"}),
    );
    assert_eq!(result["scene_id"], "scene:start");
    assert_eq!(result["entity_count"], 3);
    assert_eq!(result["returned"], 3);
    assert_eq!(result["truncated"], false);
    assert_eq!(result["path"], "scenes/start.json");
    let player = &result["entities"][0];
    assert_eq!(
        player["components"]["Transform"]["position"],
        json!([1.0, 2.0, 0.0])
    );
    assert_eq!(
        player["components"]["SpriteRenderer"]["tint"],
        json!([1.0, 1.0, 1.0, 1.0])
    );
}

#[test]
fn scene_inspect_floats_are_shortest_decimals() {
    let dir = fixture();
    fs::write(
        dir.path().join("scenes/second.json"),
        json!({"schema":1,"scene_id":"scene:second","entities":[
            {"id":"npc","name":"NPC","components":{"Transform":{"position":[0.1,0.2,0.3]}}}]})
        .to_string(),
    )
    .unwrap();
    let result = result(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/second.json"}),
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("[0.1,0.2,0.3]"), "{text}");
}

#[test]
fn scene_inspect_filters_entities_and_reports_missing_ids() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/start.json","entities":["camera","ghost"]}),
    );
    assert_eq!(result["returned"], 1);
    assert_eq!(result["entities"][0]["id"], "camera");
    assert_eq!(result["missing_entities"], json!(["ghost"]));
    assert_eq!(result["entity_count"], 3);
}

#[test]
fn scene_inspect_pages_with_offset_and_limit() {
    let dir = fixture();
    let first = data(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/start.json","limit":2}),
    );
    assert_eq!(first["returned"], 2);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["next_offset"], 2);
    let second = data(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/start.json","limit":2,"offset":2}),
    );
    assert_eq!(second["returned"], 1);
    assert_eq!(second["entities"][0]["id"], "mesh");
    assert_eq!(second["truncated"], false);
    assert!(second.get("next_offset").is_none());
}

#[test]
fn scene_inspect_validates_paging_arguments() {
    let dir = fixture();
    for args in [
        json!({"scene":"scenes/start.json","limit":0}),
        json!({"scene":"scenes/start.json","limit":1001}),
        json!({"scene":"scenes/start.json","offset":-1}),
        json!({"scene":"scenes/start.json","entities":"player"}),
        json!({"scene":"scenes/start.json","entities":[1]}),
    ] {
        assert_eq!(
            error_code(&data(dir.path(), "scene_inspect", args)),
            "invalid_argument"
        );
    }
}

#[test]
fn scene_inspect_failure_modes() {
    let dir = fixture();
    assert_eq!(
        error_code(&data(
            dir.path(),
            "scene_inspect",
            json!({"scene":"scenes/nope.json"})
        )),
        "path_invalid"
    );
    fs::write(dir.path().join("scenes/bad.json"), "{").unwrap();
    let bad = data(
        dir.path(),
        "scene_inspect",
        json!({"scene":"scenes/bad.json"}),
    );
    assert_eq!(bad["ok"], false);
    assert!(!bad["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn entity_get_returns_the_entity_with_resolved_assets() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "entity_get",
        json!({"scene":"scenes/start.json","id":"player"}),
    );
    assert_eq!(result["entity"]["id"], "player");
    assert_eq!(result["scene_id"], "scene:start");
    assert_eq!(
        result["revision"],
        revision(dir.path(), "scenes/start.json")
    );
    assert_eq!(
        result["assets"],
        json!([
            {"id":"asset:script","kind":"script","path":"src/lib.rs"},
            {"id":"asset:texture","kind":"texture","path":"assets/white.ppm"}
        ])
    );
}

#[test]
fn entity_get_unknown_id_suggests_alternatives() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "entity_get",
        json!({"scene":"scenes/start.json","id":"PLAY"}),
    );
    assert_eq!(error_code(&result), "entity_not_found");
    assert_eq!(result["error"]["details"]["suggestions"], json!(["player"]));
    assert!(result["error"]["hint"].is_string());
    let nothing = data(
        dir.path(),
        "entity_get",
        json!({"scene":"scenes/start.json","id":"zzz"}),
    );
    assert_eq!(
        nothing["error"]["details"]["suggestions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn entity_get_requires_a_string_id() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "entity_get",
        json!({"scene":"scenes/start.json","id":5}),
    );
    assert_eq!(error_code(&result), "invalid_argument");
}

// ---- component_schema -------------------------------------------------------------------

#[test]
fn component_schema_lists_all_components_and_rules() {
    let dir = fixture();
    let result = data(dir.path(), "component_schema", json!({}));
    let names: Vec<_> = result["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    let mut expected = engine_mcp::COMPONENT_NAMES.to_vec();
    expected.extend([
        "engine.AbilitySystem",
        "engine.AbsolutePosition",
        "engine.AccessibilitySettings",
        "engine.AcousticEnvironment",
        "engine.AcousticGeometry",
        "engine.AcousticMaterial",
        "engine.AcousticSource",
        "engine.AcousticVolume",
        "engine.AnimationGraphPlayer",
        "engine.AnimationPlayer",
        "engine.AudioEmitter",
        "engine.AudioListener",
        "engine.BehaviorTreeAgent",
        "engine.Buoyancy",
        "engine.CaptionPlayer",
        "engine.CharacterController2D",
        "engine.CharacterController3D",
        "engine.Cloth",
        "engine.CrowdAgent",
        "engine.DataLayer",
        "engine.Destructible3D",
        "engine.DialogueParticipant",
        "engine.EnvironmentQueries",
        "engine.FluidVolume",
        "engine.Foliage",
        "engine.Formation",
        "engine.Inventory",
        "engine.LargeWorld",
        "engine.Light2D",
        "engine.Lighting2D",
        "engine.MorphPlayer",
        "engine.MorphWeights",
        "engine.NavigationAgent",
        "engine.NavigationObstacle",
        "engine.NetworkHitboxes",
        "engine.NetworkReplica",
        "engine.NetworkReplication",
        "engine.NetworkSession",
        "engine.NetworkVoice",
        "engine.OffMeshLink",
        "engine.PcgVolume",
        "engine.PerceptionNoise",
        "engine.PerceptionSensor",
        "engine.PerceptionSettings",
        "engine.PerceptionStimulus",
        "engine.PhysicsBody2D",
        "engine.PhysicsBody3D",
        "engine.PhysicsJoint2D",
        "engine.PhysicsJoint3D",
        "engine.PhysicsSettings",
        "engine.QuestLog",
        "engine.Ragdoll",
        "engine.ScriptGraph",
        "engine.SequencePlayer",
        "engine.SkinBinding",
        "engine.SmartObject",
        "engine.SoftBody",
        "engine.SpriteLighting2D",
        "engine.StateTreeAgent",
        "engine.StreamingSource",
        "engine.Terrain",
        "engine.Tilemap",
        "engine.UiData",
        "engine.UiRoot",
        "engine.UiTheme",
        "engine.UiWorldPanel",
        "engine.Vehicle3D",
        "engine.VideoPlayer",
        "engine.WaterBody",
        "engine.WorldPartition",
        "engine.WorldText",
        "engine.XrSettings",
    ]);
    assert_eq!(names, expected);
    assert!(result["scene_rules"].as_array().unwrap().len() >= 4);
    assert_eq!(
        result["entity"]["required"],
        json!(["id", "name", "components"])
    );
}

#[test]
fn component_schema_filters_by_component() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "component_schema",
        json!({"component":"SpriteRenderer"}),
    );
    let components = result["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0]["requires"], json!(["Transform"]));
    assert_eq!(
        components[0]["schema"]["properties"]["tint"]["default"],
        json!([1, 1, 1, 1])
    );
    assert_eq!(components[0]["schema"]["additionalProperties"], false);
    assert_eq!(components[0]["schema"]["required"], json!(["texture"]));
}

#[test]
fn component_schema_rejects_unknown_components() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "component_schema",
        json!({"component":"Rigidbody"}),
    );
    assert_eq!(error_code(&result), "invalid_argument");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Transform")
    );
}

// ---- asset_list / asset_info ------------------------------------------------------------

#[test]
fn asset_list_lists_filters_and_pages() {
    let dir = fixture();
    let all = data(dir.path(), "asset_list", json!({}));
    assert_eq!(all["count"], 4);
    assert_eq!(all["assets"][0]["id"], "asset:mesh");
    assert_eq!(all["assets"][0]["path"], "assets/triangle.mesh");
    assert!(all["assets"][0]["size_bytes"].as_u64().unwrap() > 10);
    let textures = data(dir.path(), "asset_list", json!({"kind":"texture"}));
    assert_eq!(textures["count"], 1);
    assert_eq!(textures["assets"][0]["id"], "asset:texture");
    let paged = data(dir.path(), "asset_list", json!({"limit":3}));
    assert_eq!(paged["truncated"], true);
    assert_eq!(paged["next_offset"], 3);
    let rest = data(dir.path(), "asset_list", json!({"offset":3,"limit":3}));
    assert_eq!(rest["returned"], 1);
}

#[test]
fn asset_list_rejects_unknown_kinds() {
    let dir = fixture();
    let result = data(dir.path(), "asset_list", json!({"kind":"not_a_kind"}));
    assert_eq!(error_code(&result), "invalid_argument");
}

#[test]
fn authored_indirect_appears_in_asset_list_and_camera_uses() {
    let dir = fixture();
    let root = dir.path();
    let mut faces = Vec::new();
    for face in 0..6 {
        let name = format!("sky-face-{face}.rgb32f");
        fs::write(root.join("assets").join(&name), [0u8; 12]).unwrap();
        faces.push(format!("assets/{name}"));
    }
    fs::write(
        root.join("assets/sky.json"),
        serde_json::to_vec(&json!({
            "version":1,
            "environments":[{"edge":1,"diffuse":[[0,0,0],[0,0,0],[0,0,0],
                [0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0],[0,0,0]],
                "mips":[faces]}],
            "global":{"environment":0,"rotation":[0,0,0,1],"intensity":1},
            "probes":[]
        }))
        .unwrap(),
    )
    .unwrap();
    let manifest = root.join("project.json");
    let mut project: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    project["assets"].as_array_mut().unwrap().push(json!({
        "id":"asset:sky","kind":"indirect","path":"assets/sky.json"
    }));
    fs::write(&manifest, serde_json::to_vec(&project).unwrap()).unwrap();
    let scene_path = root.join("scenes/start.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
    scene["entities"].as_array_mut().unwrap().push(json!({
        "id":"ibl-camera","name":"IBL Camera","components":{
            "Transform":{},"Camera3D":{"lighting":{"indirect":"asset:sky"}}
        }
    }));
    fs::write(&scene_path, serde_json::to_vec(&scene).unwrap()).unwrap();

    let listed = data(root, "asset_list", json!({"kind":"indirect"}));
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["assets"][0]["id"], "asset:sky");
    let info = data(root, "asset_info", json!({"asset":"asset:sky"}));
    assert_eq!(info["asset"]["kind"], "indirect", "{info}");
    assert!(
        info["used_by"].as_array().unwrap().contains(&json!({
            "scene":"scenes/start.json","entity":"ibl-camera",
            "component":"Camera3D","field":"lighting.indirect"
        })),
        "{info}"
    );
}

#[test]
fn asset_info_describes_each_kind() {
    let dir = fixture();
    let texture = data(dir.path(), "asset_info", json!({"asset":"asset:texture"}));
    assert_eq!(
        texture["details"],
        json!({"width":2,"height":1,"format":"ppm","channels":"rgba8"})
    );
    let mesh = data(dir.path(), "asset_info", json!({"asset":"asset:mesh"}));
    assert_eq!(mesh["details"]["vertices"], 3);
    assert_eq!(mesh["details"]["triangles"], 1);
    assert_eq!(mesh["details"]["bounds"]["max"], json!([2.0, 3.0, 0.0]));
    let script = data(dir.path(), "asset_info", json!({"asset":"asset:script"}));
    assert_eq!(script["details"]["language"], "rust");
    assert_eq!(script["details"]["lines"], 3);
    let scene = data(dir.path(), "asset_info", json!({"asset":"asset:second"}));
    assert_eq!(scene["details"]["scene_id"], "scene:second");
    assert_eq!(scene["details"]["entities"], 1);
}

#[test]
fn asset_info_reports_png_dimensions() {
    let dir = fixture();
    let image = {
        let mut bytes = Vec::new();
        let buffer = image_bytes(5, 3);
        bytes.extend(buffer);
        bytes
    };
    fs::write(dir.path().join("assets/tile.png"), image).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(dir.path().join("project.json")).unwrap()).unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"asset:tile","kind":"texture","path":"assets/tile.png"}));
    fs::write(dir.path().join("project.json"), manifest.to_string()).unwrap();
    let info = data(dir.path(), "asset_info", json!({"asset":"asset:tile"}));
    assert_eq!(info["details"]["width"], 5);
    assert_eq!(info["details"]["height"], 3);
    assert_eq!(info["details"]["format"], "png");
}

/// A valid 5x3 RGBA PNG built by hand (stored deflate blocks).
fn image_bytes(width: u32, height: u32) -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(data);
        let mut crc = 0xffff_ffff_u32;
        for byte in kind.iter().chain(data) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        out.extend((!crc).to_be_bytes());
        out
    }
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n([255_u8, 0, 0, 255], width as usize).flatten());
    }
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend((raw.len() as u16).to_le_bytes());
    zlib.extend((!(raw.len() as u16)).to_le_bytes());
    zlib.extend(&raw);
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut header = width.to_be_bytes().to_vec();
    header.extend(height.to_be_bytes());
    header.extend([8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &header));
    png.extend(chunk(b"IDAT", &zlib));
    png.extend(chunk(b"IEND", &[]));
    png
}

#[test]
fn asset_info_lists_where_an_asset_is_used() {
    let dir = fixture();
    let info = data(dir.path(), "asset_info", json!({"asset":"asset:texture"}));
    let uses = info["used_by"].as_array().unwrap();
    assert_eq!(uses.len(), 2);
    assert!(
        uses.contains(&json!({"scene":"scenes/start.json","entity":"player",
        "component":"SpriteRenderer","field":"texture"}))
    );
    assert!(
        uses.contains(&json!({"scene":"scenes/start.json","entity":"mesh",
        "component":"MeshRenderer","field":"texture"}))
    );
    assert_eq!(info["used_by_truncated"], false);
}

#[test]
fn asset_info_reports_material_texture_dependencies() {
    let dir = fixture();
    let path = dir.path().join("scenes/start.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    scene["entities"][2]["components"]["MeshMaterial"] = json!({
        "texture":"asset:texture", "normal_texture":"asset:texture",
        "metallic_roughness_texture":"asset:texture"
    });
    fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let info = data(dir.path(), "asset_info", json!({"asset":"asset:texture"}));
    let uses = info["used_by"].as_array().unwrap();
    for field in ["texture", "normal_texture", "metallic_roughness_texture"] {
        assert!(
            uses.contains(&json!({"scene":"scenes/start.json", "entity":"mesh",
            "component":"MeshMaterial", "field":field}))
        );
    }
}

#[test]
fn asset_info_unknown_asset_is_a_structured_failure() {
    let dir = fixture();
    let result = data(dir.path(), "asset_info", json!({"asset":"asset:TEX"}));
    assert_eq!(error_code(&result), "asset_not_found");
    assert_eq!(
        result["error"]["details"]["suggestions"],
        json!(["asset:texture"])
    );
}

// ---- file_list --------------------------------------------------------------------------

fn paths(result: &Value) -> Vec<String> {
    result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn file_list_defaults_to_the_root_non_recursively() {
    let dir = fixture();
    let result = data(dir.path(), "file_list", json!({}));
    assert_eq!(result["path"], ".");
    assert_eq!(
        paths(&result),
        [
            "Cargo.toml",
            "assets",
            "notes",
            "project.json",
            "scenes",
            "src"
        ]
    );
    let kinds: Vec<_> = result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["file", "dir", "dir", "file", "dir", "dir"]);
    assert!(result["entries"][0]["size_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn file_list_subdirectory_and_recursion() {
    let dir = fixture();
    let sub = data(dir.path(), "file_list", json!({"path":"scenes"}));
    assert_eq!(sub["path"], "scenes");
    assert_eq!(paths(&sub), ["scenes/second.json", "scenes/start.json"]);
    let deep = data(dir.path(), "file_list", json!({"recursive":true}));
    assert!(paths(&deep).contains(&"notes/readme.md".to_owned()));
    assert!(paths(&deep).contains(&"notes".to_owned()));
}

#[test]
fn file_list_filters_by_extension_and_glob() {
    let dir = fixture();
    let rs = data(dir.path(), "file_list", json!({"extension":".RS"}));
    assert_eq!(paths(&rs), ["src/lib.rs"]);
    let json_files = data(dir.path(), "file_list", json!({"glob":"scenes/*.json"}));
    assert_eq!(
        paths(&json_files),
        ["scenes/second.json", "scenes/start.json"]
    );
    let by_name = data(dir.path(), "file_list", json!({"glob":"*.md"}));
    assert_eq!(paths(&by_name), ["notes/readme.md"]);
    let both = data(
        dir.path(),
        "file_list",
        json!({"glob":"**/*.json","extension":"json"}),
    );
    assert_eq!(paths(&both).len(), 3);
}

#[test]
fn file_list_pages_results() {
    let dir = fixture();
    let first = data(dir.path(), "file_list", json!({"limit":2}));
    assert_eq!(first["returned"], 2);
    assert_eq!(first["total"], 6);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["next_offset"], 2);
    let last = data(dir.path(), "file_list", json!({"limit":5,"offset":5}));
    assert_eq!(paths(&last), ["src"]);
}

#[test]
fn file_list_hides_git_derived_and_target_directories() {
    let dir = fixture();
    for hidden in [".git", ".derived", "target"] {
        fs::create_dir(dir.path().join(hidden)).unwrap();
        fs::write(dir.path().join(hidden).join("x.txt"), "x").unwrap();
    }
    let result = data(dir.path(), "file_list", json!({"recursive":true}));
    assert!(paths(&result).iter().all(|p| !p.contains("x.txt")));
    assert!(!paths(&result).contains(&".git".to_owned()));
}

#[test]
fn file_list_rejects_bad_directories() {
    let dir = fixture();
    for path in [
        "../",
        "/etc",
        "missing",
        "project.json",
        "scenes/../src",
        ".git",
        "C:/Windows",
    ] {
        let result = data(dir.path(), "file_list", json!({"path":path}));
        assert_eq!(error_code(&result), "path_invalid", "{path}");
    }
}

#[test]
fn file_list_reports_walk_truncation() {
    let dir = fixture();
    let config = engine_mcp::ServerConfig::new().with_limits(engine_mcp::Limits {
        max_walk_entries: 3,
        ..Default::default()
    });
    let result = result_with(dir.path(), config, "file_list", json!({"recursive":true}))
        ["structuredContent"]
        .clone();
    assert_eq!(result["truncated"], true);
    assert_eq!(result["total"], 3);
}

// ---- file_read --------------------------------------------------------------------------

#[test]
fn file_read_returns_content_hash_and_line_counts() {
    let dir = fixture();
    let result = data(dir.path(), "file_read", json!({"path":"notes/readme.md"}));
    assert_eq!(
        result["content"],
        "# Notes\nline two\nline three\nTODO: write docs\n"
    );
    assert_eq!(result["total_lines"], 4);
    assert_eq!(result["start_line"], 1);
    assert_eq!(result["end_line"], 4);
    assert_eq!(result["truncated"], false);
    assert_eq!(result["hash"].as_str().unwrap().len(), 64);
    assert_eq!(result["size_bytes"], 45);
}

#[test]
fn file_read_line_ranges() {
    let dir = fixture();
    let middle = data(
        dir.path(),
        "file_read",
        json!({"path":"notes/readme.md","start_line":2,"end_line":3}),
    );
    assert_eq!(middle["content"], "line two\nline three\n");
    assert_eq!(
        (middle["start_line"].as_u64(), middle["end_line"].as_u64()),
        (Some(2), Some(3))
    );
    let clamped = data(
        dir.path(),
        "file_read",
        json!({"path":"notes/readme.md","start_line":4,"end_line":99}),
    );
    assert_eq!(clamped["content"], "TODO: write docs\n");
    assert_eq!(clamped["end_line"], 4);
}

#[test]
fn file_read_range_errors() {
    let dir = fixture();
    let past = data(
        dir.path(),
        "file_read",
        json!({"path":"notes/readme.md","start_line":9}),
    );
    assert_eq!(error_code(&past), "line_out_of_range");
    let backwards = data(
        dir.path(),
        "file_read",
        json!({"path":"notes/readme.md","start_line":3,"end_line":2}),
    );
    assert_eq!(error_code(&backwards), "invalid_argument");
    let zero = data(
        dir.path(),
        "file_read",
        json!({"path":"notes/readme.md","start_line":0}),
    );
    assert_eq!(error_code(&zero), "invalid_argument");
}

#[test]
fn file_read_empty_file_and_crlf_are_preserved() {
    let dir = fixture();
    fs::write(dir.path().join("notes/empty.txt"), "").unwrap();
    let empty = data(dir.path(), "file_read", json!({"path":"notes/empty.txt"}));
    assert_eq!(empty["content"], "");
    assert_eq!(empty["total_lines"], 0);
    fs::write(dir.path().join("notes/crlf.txt"), "a\r\nb\r\n").unwrap();
    let crlf = data(dir.path(), "file_read", json!({"path":"notes/crlf.txt"}));
    assert_eq!(crlf["content"], "a\r\nb\r\n");
}

#[test]
fn file_read_truncates_at_the_read_limit_and_continues() {
    let dir = fixture();
    let lines: String = (1..=100).map(|i| format!("line number {i:03}\n")).collect();
    fs::write(dir.path().join("notes/long.txt"), &lines).unwrap();
    let config = engine_mcp::ServerConfig::new().with_limits(engine_mcp::Limits {
        max_file_read_bytes: 100,
        ..Default::default()
    });
    let first = result_with(
        dir.path(),
        config.clone(),
        "file_read",
        json!({"path":"notes/long.txt"}),
    )["structuredContent"]
        .clone();
    assert_eq!(first["truncated"], true);
    assert!(first["content"].as_str().unwrap().len() <= 100);
    let next = first["next_start_line"].as_u64().unwrap();
    assert_eq!(next, first["end_line"].as_u64().unwrap() + 1);
    let second = result_with(
        dir.path(),
        config,
        "file_read",
        json!({"path":"notes/long.txt","start_line":next}),
    )["structuredContent"]
        .clone();
    assert!(
        second["content"]
            .as_str()
            .unwrap()
            .starts_with(&format!("line number {next:03}"))
    );
}

#[test]
fn file_read_cuts_a_single_oversized_line_on_a_char_boundary() {
    let dir = fixture();
    fs::write(dir.path().join("notes/wide.txt"), "é".repeat(200)).unwrap();
    let config = engine_mcp::ServerConfig::new().with_limits(engine_mcp::Limits {
        max_file_read_bytes: 51,
        ..Default::default()
    });
    let result = result_with(
        dir.path(),
        config,
        "file_read",
        json!({"path":"notes/wide.txt"}),
    )["structuredContent"]
        .clone();
    assert_eq!(result["line_truncated"], true);
    assert_eq!(result["content"].as_str().unwrap().chars().count(), 25);
}

#[test]
fn file_read_refuses_binary_and_oversized_files() {
    let dir = fixture();
    let binary = data(dir.path(), "file_read", json!({"path":"assets/blob.bin"}));
    assert_eq!(error_code(&binary), "not_text");
    fs::write(dir.path().join("notes/invalid.txt"), [0x66_u8, 0xff, 0xfe]).unwrap();
    let invalid = data(dir.path(), "file_read", json!({"path":"notes/invalid.txt"}));
    assert_eq!(error_code(&invalid), "not_text");
    let config = engine_mcp::ServerConfig::new().with_limits(engine_mcp::Limits {
        max_file_scan_bytes: 10,
        ..Default::default()
    });
    let large = result_with(
        dir.path(),
        config,
        "file_read",
        json!({"path":"notes/readme.md"}),
    )["structuredContent"]
        .clone();
    assert_eq!(error_code(&large), "file_too_large");
    assert!(large["error"]["hint"].is_string());
}

#[test]
fn file_read_path_failures() {
    let dir = fixture();
    for path in [
        json!("../x"),
        json!("/etc/passwd"),
        json!("notes"),
        json!("missing.txt"),
        json!(".git/config"),
        json!("notes\\readme.md"),
        json!(7),
    ] {
        let result = data(dir.path(), "file_read", json!({"path":path}));
        assert_eq!(error_code(&result), "path_invalid", "{path}");
    }
}

// ---- search_text ------------------------------------------------------------------------

#[test]
fn search_text_finds_substrings_case_insensitively() {
    let dir = fixture();
    let result = data(dir.path(), "search_text", json!({"query":"todo"}));
    assert_eq!(result["match_count"], 2);
    let matches = result["matches"].as_array().unwrap();
    assert_eq!(matches[0]["path"], "notes/readme.md");
    assert_eq!(matches[0]["line"], 4);
    assert_eq!(matches[0]["column"], 1);
    assert_eq!(matches[0]["text"], "TODO: write docs");
    assert_eq!(matches[1]["path"], "src/lib.rs");
    assert_eq!(matches[1]["line"], 2);
    assert_eq!(matches[1]["column"], 4);
    assert_eq!(result["truncated"], false);
    assert!(result["skipped_binary"].as_u64().unwrap() >= 1);
}

#[test]
fn search_text_case_sensitive_regex_and_literal_metacharacters() {
    let dir = fixture();
    let sensitive = data(
        dir.path(),
        "search_text",
        json!({"query":"todo","case_sensitive":true}),
    );
    assert_eq!(sensitive["match_count"], 0);
    let regex = data(
        dir.path(),
        "search_text",
        json!({"query":"fn \\w+\\(\\)","regex":true,"extension":"rs"}),
    );
    assert_eq!(regex["match_count"], 2);
    let literal = data(dir.path(), "search_text", json!({"query":"configure()"}));
    assert_eq!(
        literal["match_count"], 1,
        "parentheses are literal without regex"
    );
}

#[test]
fn search_text_rejects_bad_patterns_and_arguments() {
    let dir = fixture();
    let bad = data(dir.path(), "search_text", json!({"query":"(","regex":true}));
    assert_eq!(error_code(&bad), "invalid_argument");
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("regular expression")
    );
    for args in [
        json!({"query":""}),
        json!({"query":"x".repeat(513)}),
        json!({"query":"x","max_results":0}),
        json!({"query":"x","max_results":501}),
        json!({"query":"x","context_lines":4}),
        json!({"query":5}),
    ] {
        assert_eq!(
            error_code(&data(dir.path(), "search_text", args)),
            "invalid_argument"
        );
    }
}

#[test]
fn search_text_caps_results_and_reports_why() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "search_text",
        json!({"query":"line","max_results":2}),
    );
    assert_eq!(result["match_count"], 2);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["truncation_reason"], "max_results");
}

#[test]
fn search_text_context_lines_and_scoping() {
    let dir = fixture();
    let result = data(
        dir.path(),
        "search_text",
        json!({"query":"line two","context_lines":1,"path":"notes"}),
    );
    let hit = &result["matches"][0];
    assert_eq!(hit["before"], json!(["# Notes"]));
    assert_eq!(hit["after"], json!(["line three"]));
    let scoped = data(
        dir.path(),
        "search_text",
        json!({"query":"line","glob":"src/**"}),
    );
    assert_eq!(scoped["match_count"], 0);
}

#[test]
fn search_text_skips_large_files_and_hidden_directories() {
    let dir = fixture();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/hidden.txt"), "needle").unwrap();
    fs::write(
        dir.path().join("notes/big.txt"),
        format!("needle{}", "x".repeat(3000)),
    )
    .unwrap();
    let config = engine_mcp::ServerConfig::new().with_limits(engine_mcp::Limits {
        max_file_scan_bytes: 2000,
        ..Default::default()
    });
    let result = result_with(dir.path(), config, "search_text", json!({"query":"needle"}))
        ["structuredContent"]
        .clone();
    assert_eq!(result["match_count"], 0);
    assert_eq!(result["skipped_large"], 1);
}

#[test]
fn search_text_clips_very_long_lines() {
    let dir = fixture();
    fs::write(
        dir.path().join("notes/long.txt"),
        format!("needle {}", "y".repeat(1000)),
    )
    .unwrap();
    let result = data(
        dir.path(),
        "search_text",
        json!({"query":"needle","path":"notes","glob":"long.txt"}),
    );
    let text = result["matches"][0]["text"].as_str().unwrap();
    assert!(text.chars().count() <= 301, "{}", text.len());
    assert!(text.ends_with('…'));
}

// ---- confinement ------------------------------------------------------------------------

#[test]
fn read_tools_never_follow_links_out_of_the_root() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP-SECRET needle").unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    for (tool, args) in [
        ("file_read", json!({"path":"escape/secret.txt"})),
        ("file_list", json!({"path":"escape"})),
        ("scene_inspect", json!({"scene":"escape/secret.txt"})),
        ("entity_get", json!({"scene":"escape/secret.txt","id":"x"})),
    ] {
        let result = data(dir.path(), tool, args);
        assert_eq!(error_code(&result), "path_invalid", "{tool}");
    }
    let listing = data(dir.path(), "file_list", json!({"recursive":true}));
    assert!(paths(&listing).iter().all(|p| !p.contains("secret")));
    let search = data(dir.path(), "search_text", json!({"query":"needle"}));
    assert!(!search.to_string().contains("TOP-SECRET"));
    assert!(
        search["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["path"] != "escape/secret.txt")
    );
}

#[test]
fn file_symlinks_are_not_listed_or_searched() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP-SECRET").unwrap();
    if !file_link(
        &outside.path().join("secret.txt"),
        &dir.path().join("notes/link.txt"),
    ) {
        return;
    }
    let listing = data(dir.path(), "file_list", json!({"path":"notes"}));
    assert!(!paths(&listing).contains(&"notes/link.txt".to_owned()));
    let read = data(dir.path(), "file_read", json!({"path":"notes/link.txt"}));
    assert_eq!(
        read["ok"], false,
        "a link to a file outside must not be readable: {read}"
    );
}

#[test]
fn tool_results_never_contain_the_host_root() {
    let dir = fixture();
    let folder = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    for (tool, args) in [
        ("project_info", json!({})),
        ("scene_list", json!({})),
        ("asset_list", json!({})),
        ("file_list", json!({"recursive":true})),
        ("file_read", json!({"path":"nope"})),
        ("asset_info", json!({"asset":"asset:texture"})),
    ] {
        let text = result(dir.path(), tool, args).to_string();
        assert!(!text.contains(&folder), "{tool} leaked the root: {text}");
    }
}

#[test]
fn read_tools_are_idempotent_and_do_not_touch_the_project() {
    let dir = fixture();
    let snapshot = || {
        let mut files: Vec<_> = walkdir(dir.path());
        files.sort();
        files
    };
    fn walkdir(path: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in fs::read_dir(path).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else {
                out.push((p.to_string_lossy().into_owned(), fs::read(&p).unwrap()));
            }
        }
        out
    }
    let before = snapshot();
    for (tool, args) in [
        ("project_info", json!({})),
        ("scene_inspect", json!({"scene":"scenes/start.json"})),
        ("asset_info", json!({"asset":"asset:texture"})),
        ("search_text", json!({"query":"line"})),
        ("file_read", json!({"path":"src/lib.rs"})),
    ] {
        let first = result(dir.path(), tool, args.clone());
        assert_eq!(first, result(dir.path(), tool, args));
    }
    assert_eq!(snapshot(), before);
}
