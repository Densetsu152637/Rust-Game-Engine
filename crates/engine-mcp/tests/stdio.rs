use engine_mcp::{
    Error, LEGACY_PROTOCOL_VERSION, MAX_MESSAGE_BYTES, MODERN_PROTOCOL_VERSION, serve,
    serve_with_limit,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufReader, Cursor, Write},
    path::Path,
};
use tempfile::TempDir;

fn metadata() -> Value {
    json!({"io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {}})
}
fn modern(id: Value, method: &str, mut params: Value) -> Value {
    params["_meta"] = metadata();
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
fn call(id: i32, name: &str, args: Value) -> Value {
    modern(
        json!(id),
        "tools/call",
        json!({"name": name, "arguments": args}),
    )
}
fn transcript(requests: Vec<Value>) -> Vec<u8> {
    requests
        .into_iter()
        .flat_map(|request| format!("{request}\n").into_bytes())
        .collect()
}
fn decode(output: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(output).unwrap();
    assert!(text.is_empty() || text.ends_with('\n'));
    text.lines()
        .map(|line| {
            assert!(line.len() <= MAX_MESSAGE_BYTES);
            serde_json::from_str(line).expect("output must contain only protocol JSON")
        })
        .collect()
}
fn run(root: &Path, requests: Vec<Value>) -> Vec<Value> {
    let mut output = Vec::new();
    serve(root, Cursor::new(transcript(requests)), &mut output).unwrap();
    decode(&output)
}
fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("scenes")).unwrap();
    fs::create_dir(dir.path().join("assets")).unwrap();
    fs::write(
        dir.path().join("project.json"),
        json!({"schema":1, "name":"MCP Fixture", "startup_scene":"scenes/start.json", "assets":[
            {"id":"asset:script", "kind":"script", "path":"assets/game.rs"},
            {"id":"asset:mesh", "kind":"mesh", "path":"assets/triangle.mesh"},
            {"id":"asset:texture", "kind":"texture", "path":"assets/white.ppm"}
        ]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.path().join("assets/game.rs"),
        "This is deliberately not compilable Rust.",
    )
    .unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='do-not-build'\nversion='0.1.0'\nbuild='build.rs'\n",
    )
    .unwrap();
    fs::write(dir.path().join("build.rs"), "fn main() { std::fs::write(\"BUILD_EXECUTED\", \"bad\").unwrap(); panic!(\"must not execute\"); }").unwrap();
    fs::write(
        dir.path().join("assets/triangle.mesh"),
        "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("assets/white.ppm"),
        "P3\n1 1\n255\n255 255 255\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("scenes/start.json"),
        json!({"schema":1,"scene_id":"scene:start","entities":[
            {"id":"entity:player","name":"Player","components":{
                "Script":{"asset":"asset:script"},
                "MeshRenderer":{"mesh":"asset:mesh","texture":"asset:texture"}
            }}
        ]})
        .to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn legacy_negotiation_lifecycle_ping_and_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let responses = run(
        dir.path(),
        vec![
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1900-01-01"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":5,"method":"initialize","params":{"protocolVersion":LEGACY_PROTOCOL_VERSION}}),
            json!({"jsonrpc":"2.0","id":6,"method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":7,"method":"missing"}),
            json!({"jsonrpc":"2.0","method":"notifications/unknown"}),
        ],
    );
    assert_eq!(responses.len(), 8);
    assert_eq!(responses[0]["error"]["code"], -32002);
    assert_eq!(
        responses[1]["result"]["protocolVersion"],
        LEGACY_PROTOCOL_VERSION
    );
    assert_eq!(
        responses[1]["result"]["serverInfo"]["name"],
        "cpp-game-engine"
    );
    assert_eq!(responses[2]["error"]["code"], -32002);
    assert_eq!(responses[3]["result"], json!({}));
    assert_eq!(
        responses[4]["result"]["tools"].as_array().unwrap().len(),
        23
    );
    assert!(responses[4]["result"].get("resultType").is_none());
    assert_eq!(responses[5]["error"]["code"], -32602);
    assert_eq!(responses[6]["error"]["code"], -32600);
    assert_eq!(responses[7]["error"]["code"], -32601);
}

#[test]
fn modern_requests_are_stateless_and_tools_have_closed_read_only_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let responses = run(
        dir.path(),
        vec![
            modern(json!(1), "server/discover", json!({})),
            modern(json!(2), "tools/list", json!({})),
            json!({"jsonrpc":"2.0","method":"tools/call","params":{"_meta":metadata()}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            modern(json!(4), "ping", json!({})),
        ],
    );
    assert_eq!(responses.len(), 4);
    assert_eq!(
        responses[0]["result"]["supportedVersions"],
        json!([MODERN_PROTOCOL_VERSION])
    );
    assert_eq!(
        responses[0]["result"]["capabilities"],
        json!({"tools":{"listChanged":false},"resources":{"subscribe":false,"listChanged":false}})
    );
    for response in &responses[..2] {
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(
            response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "cpp-game-engine"
        );
    }
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["annotations"],
            json!({"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})
        );
    }
    assert_eq!(
        find("scene_inspect")["inputSchema"]["required"],
        json!(["scene"])
    );
    assert_eq!(
        find("project_validate")["inputSchema"]["properties"]["manifest"]["type"],
        "string"
    );
    assert!(
        find("project_validate")["description"]
            .as_str()
            .unwrap()
            .contains("without compiling")
    );
    assert_eq!(responses[2]["error"]["code"], -32002);
    assert_eq!(responses[3]["error"]["code"], -32601);
}

#[test]
fn modern_metadata_errors_preserve_supported_version_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut unsupported = metadata();
    unsupported["io.modelcontextprotocol/protocolVersion"] = json!(LEGACY_PROTOCOL_VERSION);
    let responses = run(
        dir.path(),
        vec![
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":null}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":unsupported}}),
            modern(json!(5), "server/discover", json!({})),
        ],
    );
    for response in &responses[..3] {
        assert_eq!(response["error"]["code"], -32602);
    }
    assert_eq!(responses[3]["error"]["code"], -32022);
    assert_eq!(
        responses[3]["error"]["data"],
        json!({"supported":[MODERN_PROTOCOL_VERSION],"requested":LEGACY_PROTOCOL_VERSION})
    );
    assert_eq!(responses[4]["result"]["resultType"], "complete");
}

#[test]
fn malformed_envelopes_and_bounded_ids() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = vec![
        json!([]),
        json!(null),
        json!(5),
        json!({"id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1}),
        json!({"jsonrpc":"2.0","id":true,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        modern(json!("é".repeat(513)), "server/discover", json!({})),
        modern(json!("x".repeat(900_000)), "server/discover", json!({})),
    ];
    for response in run(dir.path(), invalid) {
        assert_eq!(response["error"]["code"], -32600);
        assert!(response["id"].is_null());
    }
    for id in [json!("é".repeat(512)), json!(1.5), json!(0), json!(-1)] {
        assert_eq!(
            run(
                dir.path(),
                vec![modern(id.clone(), "server/discover", json!({}))]
            )[0]["id"],
            id
        );
    }
}

#[test]
fn tool_parameters_are_strict_and_failures_are_protocol_errors() {
    let dir = tempfile::tempdir().unwrap();
    for params in [
        json!({}),
        json!({"name":false,"arguments":{}}),
        json!({"name":"scene_inspect"}),
        json!({"name":"scene_inspect","arguments":[],}),
        json!({"name":"unknown","arguments":{}}),
        json!({"name":"scene_inspect","arguments":{}}),
        json!({"name":"project_validate","arguments":{"manifest":"project.json","extra":1}}),
        json!({"name":"scene_inspect","arguments":{"scene":"start.json"},"extra":1}),
    ] {
        let responses = run(dir.path(), vec![modern(json!(1), "tools/call", params)]);
        assert_eq!(responses[0]["error"]["code"], -32602);
        assert!(responses[0].get("result").is_none());
    }
}

#[test]
fn parse_errors_recover_and_crlf_and_small_reader_buffers_work() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = b"{\n\xff\n\n".to_vec();
    bytes.extend(format!("{}\r\n", modern(json!(1), "server/discover", json!({}))).as_bytes());
    let mut output = Vec::new();
    serve(
        dir.path(),
        BufReader::with_capacity(3, Cursor::new(bytes)),
        &mut output,
    )
    .unwrap();
    let responses = decode(&output);
    assert_eq!(responses.len(), 4);
    for response in &responses[..3] {
        assert_eq!(response["error"]["code"], -32700);
    }
    assert_eq!(responses[3]["id"], 1);
}

#[test]
fn oversized_messages_terminate_with_bounded_output() {
    let dir = tempfile::tempdir().unwrap();
    for input in [b"123456789\nignored\n".as_slice(), b"123456789".as_slice()] {
        let mut output = Vec::new();
        assert!(matches!(
            serve_with_limit(
                dir.path(),
                BufReader::with_capacity(2, Cursor::new(input)),
                &mut output,
                8
            ),
            Err(Error::InvalidFraming)
        ));
        let responses = decode(&output);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("size limit")
        );
    }
    let mut output = Vec::new();
    serve_with_limit(dir.path(), Cursor::new(b"null\n"), &mut output, 4).unwrap();
    assert_eq!(decode(&output)[0]["error"]["code"], -32600);
}

#[test]
fn clean_eof_and_unterminated_messages_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let mut output = Vec::new();
    serve(dir.path(), Cursor::new([]), &mut output).unwrap();
    assert!(output.is_empty());
    assert!(matches!(
        serve(dir.path(), Cursor::new(b"{}"), &mut output),
        Err(Error::InvalidFraming)
    ));
    assert_eq!(
        decode(&output)[0]["error"]["message"],
        "unterminated JSON-RPC message"
    );
}

#[test]
fn invalid_root_and_io_failures_are_returned() {
    let dir = tempfile::tempdir().unwrap();
    let mut output = Vec::new();
    assert!(matches!(
        serve(dir.path().join("missing"), Cursor::new([]), &mut output),
        Err(Error::InvalidRoot)
    ));
    assert!(output.is_empty());
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("broken output"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(matches!(
        serve(
            dir.path(),
            Cursor::new(transcript(vec![modern(
                json!(1),
                "server/discover",
                json!({})
            )])),
            BrokenWriter
        ),
        Err(Error::Io(_))
    ));
    struct BrokenReader;
    impl io::Read for BrokenReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("broken input"))
        }
    }
    assert!(matches!(
        serve(dir.path(), BufReader::new(BrokenReader), &mut output),
        Err(Error::Io(_))
    ));
}

#[test]
fn authored_validation_and_scene_inspection_never_build_native_sources() {
    let dir = fixture();
    let before = fs::read(dir.path().join("project.json")).unwrap();
    let responses = run(
        dir.path(),
        vec![
            call(1, "project_validate", json!({"manifest":"project.json"})),
            call(2, "scene_inspect", json!({"scene":"scenes/start.json"})),
        ],
    );
    let validation = &responses[0]["result"]["structuredContent"];
    assert_eq!(validation["ok"], true);
    assert_eq!(validation["command"], "project validate");
    assert_eq!(validation["diagnostics"], json!([]));
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false);
    let scene = &result["structuredContent"];
    assert_eq!(scene["scene_id"], "scene:start");
    assert_eq!(
        scene["entities"][0]["components"]["MeshRenderer"]["mesh"],
        "asset:mesh"
    );
    assert_eq!(
        scene["entities"][0]["components"]["Script"]["asset"],
        "asset:script"
    );
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        *scene
    );
    assert_eq!(fs::read(dir.path().join("project.json")).unwrap(), before);
    assert!(!dir.path().join("BUILD_EXECUTED").exists());
    assert!(!dir.path().join("target").exists());
    assert!(!dir.path().join("Cargo.lock").exists());
}

#[test]
fn semantic_diagnostics_and_catalog_validation_are_tool_errors() {
    let dir = fixture();
    fs::write(dir.path().join("broken.json"), "{}").unwrap();
    fs::write(dir.path().join("scenes/unknown.json"), json!({"schema":1,"scene_id":"scene:unknown","entities":[{"id":"entity:x","name":"X","components":{"Script":{"asset":"missing"}}}]}).to_string()).unwrap();
    let responses = run(
        dir.path(),
        vec![
            call(1, "project_validate", json!({"manifest":"broken.json"})),
            call(2, "scene_inspect", json!({"scene":"scenes/unknown.json"})),
        ],
    );
    for response in &responses {
        assert_eq!(response["result"]["isError"], true);
        assert!(response.get("error").is_none());
        let diagnostics = response["result"]["structuredContent"]["diagnostics"]
            .as_array()
            .unwrap();
        assert!(!diagnostics.is_empty());
        for key in ["code", "severity", "file", "path", "message"] {
            assert!(diagnostics[0][key].is_string());
        }
    }
    assert_eq!(
        responses[1]["result"]["structuredContent"]["diagnostics"][0]["code"],
        "project.asset.unknown"
    );
}

#[test]
fn tool_paths_reject_missing_absolute_traversal_and_directory_arguments() {
    let dir = fixture();
    for path in [
        json!("../outside.json"),
        json!(dir.path().join("project.json")),
        json!("missing.json"),
        json!("scenes"),
        json!(""),
        json!(null),
        json!(42),
    ] {
        for (tool, field) in [("project_validate", "manifest"), ("scene_inspect", "scene")] {
            let result = run(dir.path(), vec![call(1, tool, json!({field:path}))]);
            assert_eq!(result[0]["result"]["isError"], true);
            assert!(
                result[0]["result"]["structuredContent"]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("path is invalid")
            );
        }
    }
}

fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            // Junctions enforce the same canonical reparse boundary and do not
            // require Windows' optional symlink creation privilege.
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

#[test]
fn canonical_confinement_rejects_symlink_or_junction_escape_and_catalog_escape() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.json"), "{}").unwrap();
    directory_link(outside.path(), &dir.path().join("escape"));
    let responses = run(
        dir.path(),
        vec![
            call(
                1,
                "project_validate",
                json!({"manifest":"escape/outside.json"}),
            ),
            call(2, "scene_inspect", json!({"scene":"escape/outside.json"})),
        ],
    );
    for response in responses {
        assert_eq!(response["result"]["isError"], true);
    }
    let manifest_path = dir.path().join("project.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["assets"][0]["path"] = json!("escape/outside.json");
    fs::write(manifest_path, manifest.to_string()).unwrap();
    let response = run(
        dir.path(),
        vec![call(
            1,
            "project_validate",
            json!({"manifest":"project.json"}),
        )],
    );
    assert_eq!(
        response[0]["result"]["structuredContent"]["diagnostics"][0]["code"],
        "project.path.outside_root"
    );
}

#[test]
fn oversized_scene_results_remain_valid_bounded_tool_errors() {
    let dir = fixture();
    let frames: Vec<_> = (0..4096).map(|_| json!([0, 0, 1, 1])).collect();
    let entities: Vec<_> = (0..200)
        .map(|i| {
            json!({"id":format!("entity:{i}"), "name":"x", "components":{
                "Transform":{"position":[0,0,0]},
                "SpriteRenderer":{"texture":"asset:texture","frames":frames}}})
        })
        .collect();
    fs::write(
        dir.path().join("scenes/large.json"),
        json!({"schema":1,"scene_id":"scene:large","entities":entities}).to_string(),
    )
    .unwrap();
    let responses = run(
        dir.path(),
        vec![
            call(1, "scene_inspect", json!({"scene":"scenes/large.json"})),
            modern(json!(2), "server/discover", json!({})),
        ],
    );
    assert_eq!(responses[0]["result"]["isError"], true);
    assert_eq!(
        responses[0]["result"]["structuredContent"]["error"]["message"],
        "tool response exceeds the 1 MiB output limit"
    );
    assert_eq!(responses[0]["result"]["resultType"], "complete");
    assert_eq!(responses[1]["id"], 2);
}
