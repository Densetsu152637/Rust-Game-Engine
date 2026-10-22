#![allow(dead_code)]
//! Shared helpers for the tool, write, resource and provider test suites: a
//! rich fixture project and a tiny in-memory client.

use engine_mcp::{MODERN_PROTOCOL_VERSION, ServerConfig, serve_with_config};
use serde_json::{Value, json};
use std::{fs, io::Cursor, path::Path};
use tempfile::TempDir;

pub const META: &str = "io.modelcontextprotocol/protocolVersion";
pub const CAPS: &str = "io.modelcontextprotocol/clientCapabilities";

pub fn modern(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({META: MODERN_PROTOCOL_VERSION, CAPS: {}});
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

pub fn read_only() -> ServerConfig {
    ServerConfig::new()
}
pub fn writable() -> ServerConfig {
    ServerConfig::new().allow_write(true)
}

/// Sends the requests in one session and returns every response.
pub fn exchange(root: &Path, config: ServerConfig, requests: &[Value]) -> Vec<Value> {
    let input: Vec<u8> = requests
        .iter()
        .flat_map(|r| format!("{r}\n").into_bytes())
        .collect();
    let mut output = Vec::new();
    serve_with_config(root, Cursor::new(input), &mut output, config).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout carries JSON-RPC only"))
        .collect()
}

/// Calls one tool on a fresh read-only server and returns the whole result.
pub fn result(root: &Path, name: &str, args: Value) -> Value {
    result_with(root, read_only(), name, args)
}

pub fn result_with(root: &Path, config: ServerConfig, name: &str, args: Value) -> Value {
    let mut responses = exchange(
        root,
        config,
        &[modern(
            1,
            "tools/call",
            json!({"name": name, "arguments": args}),
        )],
    );
    let response = responses.remove(0);
    assert!(
        response.get("error").is_none(),
        "unexpected protocol error: {response}"
    );
    response["result"].clone()
}

/// `structuredContent` of a read-only call.
pub fn data(root: &Path, name: &str, args: Value) -> Value {
    result(root, name, args)["structuredContent"].clone()
}

/// `structuredContent` of a call on a write-enabled server.
pub fn write(root: &Path, name: &str, args: Value) -> Value {
    result_with(root, writable(), name, args)["structuredContent"].clone()
}

pub fn error_code(data: &Value) -> String {
    assert_eq!(data["ok"], false, "expected a failure: {data}");
    data["error"]["code"].as_str().unwrap().to_owned()
}

/// A project with two scenes, a script crate, assets and input bindings.
pub fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for sub in ["scenes", "assets", "src", "notes"] {
        fs::create_dir(root.join(sub)).unwrap();
    }
    fs::write(
        root.join("project.json"),
        json!({"schema":1,"name":"Tool Fixture","rust":{"manifest":"Cargo.toml"},
            "inputActions":{"jump":"Space"},
            "inputBindings":{"move_left":["Key:A","Key:Left"]},
            "startup_scene":"scenes/start.json","assets":[
            {"id":"asset:script","kind":"script","path":"src/lib.rs"},
            {"id":"asset:mesh","kind":"mesh","path":"assets/triangle.mesh"},
            {"id":"asset:texture","kind":"texture","path":"assets/white.ppm"},
            {"id":"asset:second","kind":"scene","path":"scenes/second.json"}]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"tool-fixture\"\nversion = \"0.1.0\"\n\n[dependencies]\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn configure() {}\n// TODO: add gameplay\nfn helper() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("assets/triangle.mesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 2 0 0 1 0\nvertex 0 3 0 0 1\n",
    )
    .unwrap();
    fs::write(
        root.join("assets/white.ppm"),
        "P3\n2 1\n255\n255 255 255\n0 0 0\n",
    )
    .unwrap();
    fs::write(root.join("assets/blob.bin"), [0_u8, 1, 2, 255, 0, 7]).unwrap();
    fs::write(
        root.join("notes/readme.md"),
        "# Notes\nline two\nline three\nTODO: write docs\n",
    )
    .unwrap();
    fs::write(
        root.join("scenes/start.json"),
        json!({"schema":1,"scene_id":"scene:start","entities":[
            {"id":"player","name":"Player","components":{
                "Transform":{"position":[1.0,2.0,0.0]},
                "Script":{"asset":"asset:script"},
                "SpriteRenderer":{"texture":"asset:texture","size":[1,1]},
                "Collider2D":{"size":[1,1]}}},
            {"id":"camera","name":"Camera","components":{
                "Transform":{"position":[0,0,0]},
                "Camera2D":{"follow":"player"}}},
            {"id":"mesh","name":"Mesh","components":{
                "MeshRenderer":{"mesh":"asset:mesh","texture":"asset:texture"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("scenes/second.json"),
        json!({"schema":1,"scene_id":"scene:second","entities":[
            {"id":"npc","name":"NPC","components":{"Transform":{"position":[5,5,0]}}}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

/// Revision of a scene as reported by `scene_inspect`.
pub fn revision(root: &Path, scene: &str) -> String {
    data(root, "scene_inspect", json!({"scene": scene, "limit": 1}))["revision"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Creates a directory link (symlink, or a junction on Windows).
pub fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction creation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// Creates a file symlink; returns false when the platform refuses (Windows
/// without the privilege), in which case the caller skips the check.
pub fn file_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}
