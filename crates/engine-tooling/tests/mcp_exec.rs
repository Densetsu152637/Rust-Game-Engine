//! `engine mcp` end to end: CLI flags, gating, and the execution tools
//! (`project_build`, `project_run_headless`, `render_screenshot`) against real
//! native generations. Dependency output is shared with the other pipeline tests
//! in `target/native-generation-shared`. The server runs as the real binary so
//! stdout cleanliness is checked too.

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const META: &str = "io.modelcontextprotocol/protocolVersion";
const CAPS: &str = "io.modelcontextprotocol/clientCapabilities";

fn engine_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn shared_target() -> PathBuf {
    engine_root().join("target/native-generation-shared")
}
fn engine_path(name: &str) -> String {
    engine_root()
        .join("crates")
        .join(name)
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

struct Session {
    responses: Vec<Value>,
    stderr: String,
    code: i32,
}

fn request(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({META: "2026-07-28", CAPS: {}});
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn call(id: i64, name: &str, arguments: Value) -> Value {
    request(
        id,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

/// Starts `engine mcp <args>`, sends the requests, closes stdin and collects
/// the output. Every stdout line must be a JSON-RPC message.
fn mcp(args: &[&std::ffi::OsStr], envs: &[(&str, &str)], requests: &[Value]) -> Session {
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine"))
        .arg("mcp")
        .args(args)
        .env("ENGINE_NATIVE_TARGET_DIR", shared_target())
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses = stdout
        .lines()
        .map(|line| {
            let message: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stdout must be JSON-RPC only: {e}: {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            message
        })
        .collect();
    Session {
        responses,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

fn os(text: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(text)
}

const PLAYER: &str = r#"
    use engine_core::bevy_ecs::prelude::*;
    use engine_project::{NativeScript,Transform,InputSnapshot};
    struct Player;
    impl NativeScript for Player {
        fn on_create(&mut self,world:&mut World,owner:Entity)->engine_project::Result<()> {
            let x=world.get::<Transform>(owner).unwrap().position[0];
            world.resource_mut::<engine_project::runtime::RuntimeLog>().0.push(format!("created at {x}"));
            Ok(())
        }
        fn on_update(&mut self,world:&mut World,owner:Entity,dt:f32)->engine_project::Result<()> {
            if world.resource::<InputSnapshot>().held("move_right") {world.get_mut::<Transform>(owner).unwrap().position[0]+=2.0*dt;}
            std::thread::sleep(std::time::Duration::from_millis(1));
            Ok(())
        }
        fn on_event(&mut self,world:&mut World,owner:Entity,event:&engine_project::EngineEvent)->engine_project::Result<()> {
            if event.name=="test.bump" {
                world.get_mut::<Transform>(owner).unwrap().position[0]+=event.payload["delta"].as_f64().unwrap() as f32;
            }
            Ok(())
        }
        fn on_destroy(&mut self,world:&mut World,_:Entity)->engine_project::Result<()> { world.resource_mut::<engine_project::runtime::RuntimeLog>().0.push("clean native teardown".into()); Ok(()) }
    }
    pub fn configure(app:&mut engine_core::EngineApp)->engine_core::Result<()> {
        engine_project::register_script(app,"asset:player",|_,_|Ok(Box::new(Player)))?;Ok(())
    }
"#;

/// A native game project in a fresh directory under the engine's target dir.
fn game(prefix: &str) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(engine_root().join("target"))
        .unwrap();
    let root = temp.path().join("game");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname='mcp-exec-game'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[dependencies]\nengine-core={{path={:?}}}\nengine-project={{path={:?}}}\n",
            engine_path("engine-core"),
            engine_path("engine-project")
        ),
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), PLAYER).unwrap();
    fs::write(
        root.join("project.json"),
        r#"{"schema":1,"name":"mcp exec","startup_scene":"main.scene.json","rust":{"manifest":"Cargo.toml"},"inputActions":{"move_right":"Right"},"assets":[{"id":"asset:player","kind":"script","path":"src/lib.rs"}]}"#,
    )
    .unwrap();
    fs::write(
        root.join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"entity:player","name":"Player","components":{"Transform":{"position":[0,0,0]},"Script":{"asset":"asset:player"}}}]}"#,
    )
    .unwrap();
    (temp, root)
}

fn result_of(session: &Session, index: usize) -> &Value {
    let response = &session.responses[index];
    assert!(
        response.get("error").is_none(),
        "protocol error: {response}"
    );
    &response["result"]["structuredContent"]
}

fn tool_names(session: &Session) -> Vec<String> {
    session.responses[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

const EXEC_TOOLS: [&str; 3] = ["project_build", "project_run_headless", "render_screenshot"];

// ---- CLI flags and gating (no builds) -------------------------------------------------------

#[test]
fn default_server_is_read_only_and_lists_no_write_or_exec_tools() {
    let (_temp, root) = game("mcp-cli-");
    let session = mcp(
        &[root.as_os_str()],
        &[],
        &[request(1, "tools/list", json!({}))],
    );
    assert_eq!(session.code, 0, "{}", session.stderr);
    let names = tool_names(&session);
    assert_eq!(names.len(), 12);
    for hidden in EXEC_TOOLS.iter().chain(&["file_write", "entity_create"]) {
        assert!(!names.contains(&(*hidden).to_owned()), "{hidden}");
    }
}

/// Logging is on at debug level, yet stdout stays pure JSON-RPC (the `mcp`
/// helper rejects any non-JSON-RPC stdout line) and the log events reach stderr.
#[test]
fn debug_logging_keeps_mcp_stdout_pure_json_rpc() {
    let (_temp, root) = game("mcp-log-");
    let session = mcp(
        &[root.as_os_str()],
        &[("ENGINE_LOG", "debug")],
        &[request(1, "tools/list", json!({}))],
    );
    assert_eq!(session.code, 0, "{}", session.stderr);
    assert_eq!(session.responses.len(), 1);
    assert_eq!(session.responses[0]["id"], 1);
    assert!(
        session.stderr.contains("MCP session started"),
        "{}",
        session.stderr
    );
    assert!(
        session.stderr.contains("MCP session ended"),
        "{}",
        session.stderr
    );
}

#[test]
fn allow_write_adds_only_the_write_tools() {
    let (_temp, root) = game("mcp-cli-");
    let session = mcp(
        &[root.as_os_str(), os("--allow-write")],
        &[],
        &[request(1, "tools/list", json!({}))],
    );
    let names = tool_names(&session);
    assert_eq!(names.len(), 18);
    assert!(names.contains(&"entity_create".to_owned()));
    assert!(EXEC_TOOLS.iter().all(|t| !names.contains(&(*t).to_owned())));
}

#[test]
fn allow_exec_adds_the_execution_tools_with_accurate_metadata() {
    let (_temp, root) = game("mcp-cli-");
    let session = mcp(
        &[os("--allow-exec"), root.as_os_str()],
        &[],
        &[request(1, "tools/list", json!({}))],
    );
    let names = tool_names(&session);
    assert_eq!(names.len(), 15, "no write tools without --allow-write");
    assert_eq!(names[12..], EXEC_TOOLS);
    assert!(!names.contains(&"file_write".to_owned()));
    let tools = session.responses[0]["result"]["tools"].as_array().unwrap();
    for tool in &tools[12..] {
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["openWorldHint"], true);
        assert!(tool["description"].as_str().unwrap().len() > 100);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["outputSchema"].is_object());
    }
    assert_eq!(tools[12]["annotations"]["destructiveHint"], false);
    assert_eq!(tools[12]["annotations"]["idempotentHint"], true);
    assert_eq!(
        tools[13]["annotations"]["destructiveHint"], true,
        "runs arbitrary native code"
    );
}

#[test]
fn both_flags_list_everything() {
    let (_temp, root) = game("mcp-cli-");
    let session = mcp(
        &[root.as_os_str(), os("--allow-write"), os("--allow-exec")],
        &[],
        &[request(1, "tools/list", json!({}))],
    );
    assert_eq!(tool_names(&session).len(), 21);
}

#[test]
fn exec_tools_are_rejected_without_the_flag() {
    let (_temp, root) = game("mcp-cli-");
    let session = mcp(
        &[root.as_os_str(), os("--allow-write")],
        &[],
        &[
            call(1, "project_build", json!({})),
            call(2, "project_run_headless", json!({})),
            call(3, "render_screenshot", json!({})),
        ],
    );
    for response in &session.responses {
        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("exec access")
        );
    }
    assert!(!root.join(".derived").exists(), "nothing was built");
}

#[test]
fn invalid_command_lines_exit_with_usage_errors() {
    let (_temp, root) = game("mcp-cli-");
    for args in [
        vec![],
        vec![os("--allow-write")],
        vec![root.as_os_str(), os("--bogus")],
        vec![root.as_os_str(), os("--allow-write"), os("--allow-write")],
        vec![root.as_os_str(), os("--allow-exec"), os("--allow-exec")],
        vec![root.as_os_str(), root.as_os_str()],
        vec![root.as_os_str(), os("--format"), os("json")],
    ] {
        let session = mcp(&args, &[], &[]);
        assert_eq!(session.code, 2, "{args:?}: {}", session.stderr);
        assert!(
            session
                .stderr
                .contains("Usage: engine mcp <project-root> [--allow-write] [--allow-exec]")
        );
        assert!(
            session.responses.is_empty(),
            "stdout stays empty on usage errors"
        );
    }
}

#[test]
fn a_missing_root_is_an_error_on_stderr_only() {
    let session = mcp(&[os("this-directory-does-not-exist")], &[], &[]);
    assert_eq!(session.code, 1);
    assert!(session.stderr.contains("MCP:"));
    assert!(session.responses.is_empty());
}

#[test]
fn write_access_edits_a_real_project_over_stdio() {
    let (_temp, root) = game("mcp-write-");
    let before = fs::read(root.join("main.scene.json")).unwrap();
    let inspect = mcp(
        &[root.as_os_str(), os("--allow-write")],
        &[],
        &[call(1, "scene_inspect", json!({"scene":"main.scene.json"}))],
    );
    let revision = result_of(&inspect, 0)["revision"].clone();
    let edit = mcp(
        &[root.as_os_str(), os("--allow-write")],
        &[],
        &[call(
            1,
            "entity_create",
            json!({"scene":"main.scene.json","expected_revision":revision,"id":"entity:crate",
                "components":{"Transform":{"position":[2,0,0]}}}),
        )],
    );
    assert_eq!(result_of(&edit, 0)["ok"], true, "{}", edit.responses[0]);
    assert_ne!(fs::read(root.join("main.scene.json")).unwrap(), before);
    let again = mcp(
        &[root.as_os_str()],
        &[],
        &[call(
            1,
            "entity_create",
            json!({"scene":"main.scene.json","expected_revision":"x","id":"e"}),
        )],
    );
    assert_eq!(
        again.responses[0]["error"]["code"], -32602,
        "read-only server rejects writes"
    );
}

// ---- execution tools (real native builds) -----------------------------------------------------

fn generation_dirs(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(root.join(".derived/native"))
        .map(|dir| {
            dir.flatten()
                .filter(|e| e.path().join("generation.json").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn build_run_input_reuse_failure_and_timeouts_over_stdio() {
    let (_temp, root) = game("mcp-exec-");
    let exec = [root.as_os_str(), os("--allow-exec")];

    // 1. project_build compiles a generation and reports it.
    let built = mcp(&exec, &[], &[call(1, "project_build", json!({}))]);
    let build = result_of(&built, 0);
    assert_eq!(build["ok"], true, "{}\n{}", build, built.stderr);
    assert_eq!(build["profile"], "dev");
    let generation = build["generation"].as_str().unwrap().to_owned();
    assert_eq!(build["fingerprint"].as_str().unwrap().len(), 64);
    let executable = build["executable"].as_str().unwrap();
    assert!(executable.starts_with(".derived/native/"), "{executable}");
    assert!(root.join(executable).is_file());
    assert_eq!(build["warning_count"], 0);
    assert_eq!(generation_dirs(&root), std::slice::from_ref(&generation));
    assert!(
        !built.responses[0]
            .to_string()
            .contains(&root.to_string_lossy().replace('\\', "\\\\"))
    );

    // 2. Run headless with scripted input: 30 held ticks at 2 units/s = 1 unit.
    let ran = mcp(
        &exec,
        &[],
        &[call(
            1,
            "project_run_headless",
            json!({"ticks":30,"inputs":[{"action":"move_right","tick":0,"duration":30}],
                "rebuild":false}),
        )],
    );
    // A fresh server process has no remembered build, so it builds once itself.
    let run = result_of(&ran, 0);
    assert_eq!(run["ok"], true, "{}\n{}", run, ran.stderr);
    assert_eq!(run["ticks"], 30);
    assert_eq!(run["scene_id"], "scene:main");
    assert_eq!(run["entity_count"], 1);
    let x = run["entities"][0]["components"]["Transform"]["position"][0]
        .as_f64()
        .unwrap();
    assert!((x - 1.0).abs() < 1e-4, "x = {x}");
    let logs: Vec<_> = run["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(logs.contains(&"created at 0"), "{logs:?}");
    assert!(logs.contains(&"clean native teardown"), "{logs:?}");
    assert_eq!(run["logs_truncated"], false);
    assert_eq!(run["entities_truncated"], false);

    // 3. One server session: build once, reuse it, filter entities and invoke a native callback.
    let authored_before = fs::read(root.join("main.scene.json")).unwrap();
    let session = mcp(
        &exec,
        &[],
        &[
            call(1, "project_build", json!({})),
            call(
                2,
                "project_run_headless",
                json!({"ticks":3,"rebuild":false,"entities":["entity:player"]}),
            ),
            call(
                3,
                "project_run_headless",
                json!({"ticks":3,"rebuild":false,"entities":["nobody"]}),
            ),
            call(
                4,
                "project_run_headless",
                json!({"ticks":3,"rebuild":false,"events":[{
                    "tick":0,"name":"test.bump","target":"entity:player","payload":{"delta":7}
                }]}),
            ),
        ],
    );
    assert_eq!(result_of(&session, 0)["ok"], true);
    let reused = result_of(&session, 1);
    assert_eq!(reused["build"]["reused"], true);
    assert_eq!(
        reused["build"]["generation"],
        result_of(&session, 0)["generation"]
    );
    assert_eq!(reused["entities"][0]["id"], "entity:player");
    assert_eq!(result_of(&session, 2)["entities"], json!([]));
    assert_eq!(result_of(&session, 2)["entity_count"], 1);
    let scheduled = result_of(&session, 3);
    assert_eq!(scheduled["ok"], true, "{scheduled}");
    assert_eq!(
        scheduled["entities"][0]["components"]["Transform"]["position"][0],
        7.0
    );
    assert_eq!(scheduled["event_count"], 1);
    assert_eq!(scheduled["event_trace"][0]["name"], "test.bump");
    assert_eq!(scheduled["event_trace"][0]["tick"], 0);
    assert_eq!(scheduled["event_trace_truncated"], false);
    assert_eq!(
        fs::read(root.join("main.scene.json")).unwrap(),
        authored_before
    );

    // 4. Argument validation yields structured tool errors, not protocol errors.
    let bad = mcp(
        &exec,
        &[],
        &[
            call(
                1,
                "project_run_headless",
                json!({"inputs":[{"action":"fly","tick":0}],"rebuild":false}),
            ),
            call(
                2,
                "project_run_headless",
                json!({"ticks":5,"inputs":[{"action":"move_right","tick":9}]}),
            ),
            call(3, "project_run_headless", json!({"ticks":0})),
            call(
                4,
                "project_run_headless",
                json!({"scene":"../outside.json"}),
            ),
            call(5, "project_run_headless", json!({"inputs":"jump"})),
        ],
    );
    let code = |i: usize| {
        result_of(&bad, i)["error"]["code"]
            .as_str()
            .unwrap_or("?")
            .to_owned()
    };
    assert_eq!(code(0), "unknown_action");
    assert_eq!(
        result_of(&bad, 0)["error"]["details"]["declared"],
        json!(["move_right"])
    );
    assert_eq!(code(1), "invalid_argument");
    assert_eq!(code(2), "invalid_argument");
    assert_eq!(code(3), "path_invalid");
    assert_eq!(code(4), "invalid_argument");

    // 5. A failing build reports compiler diagnostics and keeps the last good generation.
    let generations_before = generation_dirs(&root);
    assert!(!generations_before.is_empty() && generations_before.len() <= 3);
    let good_source = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        format!("{good_source}\nfn broken() {{ let x: u32 = \"no\"; }}\n"),
    )
    .unwrap();
    let failed = mcp(
        &exec,
        &[],
        &[
            call(1, "project_build", json!({})),
            call(2, "project_run_headless", json!({"ticks":1})),
        ],
    );
    let failure = result_of(&failed, 0);
    assert_eq!(failure["ok"], false, "{failure}");
    assert_eq!(failure["error"]["code"], "compiler.build");
    assert!(
        failure["error"]["message"]
            .as_str()
            .unwrap()
            .contains("error")
    );
    let diagnostic = &failure["diagnostics"][0];
    assert_eq!(diagnostic["severity"], "error");
    assert!(
        diagnostic["code"].as_str().unwrap().starts_with("rust."),
        "{diagnostic}"
    );
    assert!(
        diagnostic["file"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("src/lib.rs"),
        "{diagnostic}"
    );
    assert!(diagnostic["path"].as_str().unwrap().contains(':'));
    assert!(
        diagnostic["rendered"]
            .as_str()
            .unwrap()
            .contains("mismatched types")
    );
    let last_good = failure["error"]["details"]["last_good_generation"]
        .as_str()
        .unwrap();
    assert!(
        generations_before.iter().any(|g| g == last_good),
        "{last_good} must be one of the generations built before the failure"
    );
    assert!(
        failure["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("Earlier generations are untouched")
    );
    assert_eq!(
        generation_dirs(&root),
        generations_before,
        "every good generation survives and no partial one remains"
    );
    assert!(
        root.join(".derived/native")
            .join(last_good)
            .join(if cfg!(windows) {
                "runtime.exe"
            } else {
                "runtime"
            })
            .is_file()
    );
    assert_eq!(
        result_of(&failed, 1)["error"]["code"],
        "compiler.build",
        "runs surface build failures too"
    );
    assert!(!failed.responses.iter().any(|r| {
        r.to_string()
            .contains(&root.to_string_lossy().replace('\\', "\\\\"))
    }));
    fs::write(root.join("src/lib.rs"), &good_source).unwrap();

    // 6. A run that exceeds its time limit is stopped with a structured error.
    let slow = mcp(
        &exec,
        &[],
        &[call(
            1,
            "project_run_headless",
            json!({"ticks":100000,"timeout_seconds":5}),
        )],
    );
    let timeout = result_of(&slow, 0);
    assert_eq!(timeout["ok"], false, "{timeout}");
    assert_eq!(timeout["error"]["code"], "run_timeout");
    assert!(timeout["error"]["hint"].is_string());

    // 7. Without a Rust crate the exec tools explain what is missing.
    let (_other, content_only) = game("mcp-exec-nocrate-");
    fs::remove_file(content_only.join("Cargo.toml")).unwrap();
    fs::write(
        content_only.join("project.json"),
        r#"{"schema":1,"name":"content","startup_scene":"empty.scene.json","assets":[]}"#,
    )
    .unwrap();
    fs::write(
        content_only.join("empty.scene.json"),
        r#"{"schema":1,"scene_id":"scene:empty","entities":[]}"#,
    )
    .unwrap();
    let none = mcp(
        &[content_only.as_os_str(), os("--allow-exec")],
        &[],
        &[call(1, "project_build", json!({}))],
    );
    assert_eq!(result_of(&none, 0)["error"]["code"], "rust_crate_missing");
}

#[test]
fn a_short_build_timeout_cancels_without_leaving_a_generation() {
    let (_temp, root) = game("mcp-exec-timeout-");
    let session = mcp(
        &[root.as_os_str(), os("--allow-exec")],
        &[],
        &[call(1, "project_build", json!({"timeout_seconds":10}))],
    );
    let response = result_of(&session, 0);
    // The shared dependency cache may already be warm, in which case the build
    // can finish inside the limit; either outcome must leave a consistent cache.
    if response["ok"] == false {
        assert_eq!(response["error"]["code"], "build_timeout", "{response}");
        assert!(generation_dirs(&root).is_empty());
    } else {
        assert_eq!(generation_dirs(&root).len(), 1);
    }
}

#[test]
#[ignore = "requires a real native wgpu adapter; run cargo test -p engine-tooling --test mcp_exec -- --ignored"]
fn render_screenshot_returns_a_png_image_over_stdio() {
    let (_temp, root) = game("mcp-shot-");
    // A red 1x1 texture drawn as a 4x4 unit sprite at the origin.
    fs::write(root.join("red.ppm"), "P3\n1 1\n255\n255 0 0\n").unwrap();
    fs::write(
        root.join("project.json"),
        r#"{"schema":1,"name":"mcp shot","startup_scene":"main.scene.json","rust":{"manifest":"Cargo.toml"},"inputActions":{"move_right":"Right"},"assets":[{"id":"asset:player","kind":"script","path":"src/lib.rs"},{"id":"asset:red","kind":"texture","path":"red.ppm"}]}"#,
    )
    .unwrap();
    fs::write(
        root.join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"entity:player","name":"Player","components":{"Transform":{"position":[0,0,0]},"Script":{"asset":"asset:player"}}},{"id":"entity:sprite","name":"Sprite","components":{"Transform":{"position":[0,0,0]},"SpriteRenderer":{"texture":"asset:red","size":[4,4]}}}]}"#,
    )
    .unwrap();
    let session = mcp(
        &[root.as_os_str(), os("--allow-exec")],
        &[],
        &[call(
            1,
            "render_screenshot",
            json!({"ticks":2,"width":160,"height":90,
                "inputs":[{"action":"move_right","tick":0,"duration":2}]}),
        )],
    );
    let result = &session.responses[0]["result"];
    let data = &result["structuredContent"];
    assert_eq!(data["ok"], true, "{data}\n{}", session.stderr);
    assert_eq!(
        (data["width"].as_u64(), data["height"].as_u64()),
        (Some(160), Some(90))
    );
    assert!(data["adapter"]["name"].is_string());
    let image = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .expect("an image content block");
    assert_eq!(image["mimeType"], "image/png");
    let png = base64_decode(image["data"].as_str().unwrap());
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(png.len() as u64, data["png_bytes"].as_u64().unwrap());
    let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (160, 90));
    assert_eq!(
        decoded.get_pixel(80, 45).0,
        [255, 0, 0, 255],
        "the sprite is drawn at the centre"
    );
    assert_eq!(
        decoded.get_pixel(2, 2).0,
        [0, 0, 0, 255],
        "the background is cleared to black"
    );
}

#[test]
fn render_screenshot_validates_its_arguments_before_building_anything() {
    // The structured `gpu_unavailable` mapping is unit-tested in `mcp_tools`;
    // here every rejected call must fail fast, before any compilation.
    let (_temp, root) = game("mcp-shot-args-");
    let session = mcp(
        &[root.as_os_str(), os("--allow-exec")],
        &[],
        &[
            call(1, "render_screenshot", json!({"width": 8})),
            call(2, "render_screenshot", json!({"height": 4096})),
            call(3, "render_screenshot", json!({"ticks": 0})),
            call(4, "render_screenshot", json!({"scene": "../x.json"})),
            call(
                5,
                "render_screenshot",
                json!({"inputs": [{"action": "fly", "tick": 0}]}),
            ),
            call(6, "render_screenshot", json!({"width": "wide"})),
        ],
    );
    let codes: Vec<_> = (0..6)
        .map(|i| {
            result_of(&session, i)["error"]["code"]
                .as_str()
                .unwrap_or("?")
                .to_owned()
        })
        .collect();
    assert_eq!(
        codes,
        [
            "invalid_argument",
            "invalid_argument",
            "invalid_argument",
            "path_invalid",
            "unknown_action",
            "invalid_argument"
        ]
    );
    assert!(!root.join(".derived").exists(), "nothing was compiled");
}

/// Every spelling of the project root (verbatim, plain, forward slashes and the
/// JSON-escaped forms) that must never appear in a result.
fn root_spellings(root: &Path) -> Vec<String> {
    let canonical = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let plain = canonical.trim_start_matches(r"\\?\").to_owned();
    let mut all = vec![
        canonical.clone(),
        plain.clone(),
        plain.replace('\\', "/"),
        plain.replace('\\', r"\\"),
        canonical.replace('\\', r"\\"),
    ];
    all.retain(|s| !s.is_empty());
    all
}

fn assert_no_host_paths(text: &str, root: &Path) {
    for spelling in root_spellings(root) {
        assert!(!text.contains(&spelling), "{spelling} leaked:\n{text}");
    }
}

/// `project_build` reported warning files and rendered `-->` paths as absolute
/// host paths on success, and failures carried cargo and temp paths in
/// `stderr_tail`. Results must be project-relative everywhere.
#[test]
fn build_results_never_carry_host_paths() {
    let (_temp, root) = game("mcp-scrub-");
    let exec = [root.as_os_str(), os("--allow-exec")];
    let good = fs::read_to_string(root.join("src/lib.rs")).unwrap();

    // Success with a warning.
    fs::write(
        root.join("src/lib.rs"),
        format!("{good}\n#[allow(dead_code)]\nfn unused_helper() {{ let unused_variable = 1; }}\n"),
    )
    .unwrap();
    let built = mcp(&exec, &[], &[call(1, "project_build", json!({}))]);
    let build = result_of(&built, 0);
    assert_eq!(build["ok"], true, "{build}\n{}", built.stderr);
    assert!(build["warning_count"].as_u64().unwrap() >= 1, "{build}");
    assert_no_host_paths(&built.responses[0].to_string(), &root);
    for warning in build["warnings"].as_array().unwrap() {
        let file = warning["file"].as_str().unwrap();
        assert_eq!(file, "src/lib.rs", "{warning}");
        let rendered = warning["rendered"].as_str().unwrap();
        assert!(rendered.contains("--> src/lib.rs"), "{rendered}");
    }

    // Failure with a compiler error: diagnostics and stderr_tail.
    fs::write(
        root.join("src/lib.rs"),
        format!("{good}\nfn broken() {{ let x: u32 = \"no\"; }}\n"),
    )
    .unwrap();
    let failed = mcp(&exec, &[], &[call(1, "project_build", json!({}))]);
    let failure = result_of(&failed, 0);
    assert_eq!(failure["ok"], false, "{failure}");
    assert_no_host_paths(&failed.responses[0].to_string(), &root);
    let diagnostic = &failure["diagnostics"][0];
    assert_eq!(diagnostic["file"], "src/lib.rs", "{failure}");
    assert!(
        diagnostic["rendered"]
            .as_str()
            .unwrap()
            .contains("--> src/lib.rs:"),
        "{failure}"
    );
}

/// Output above the 8 MiB capture cap loses the run envelope. That must be
/// reported as `output_too_large`, not as a missing result.
#[test]
fn a_run_that_prints_too_much_is_output_too_large() {
    let (_temp, root) = game("mcp-flood-");
    let source = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        source.replace(
            "let x=world.get::<Transform>(owner).unwrap().position[0];",
            "let x=world.get::<Transform>(owner).unwrap().position[0];\n            println!(\"{}\", \"x\".repeat(9 * 1024 * 1024));",
        ),
    )
    .unwrap();
    let session = mcp(
        &[root.as_os_str(), os("--allow-exec")],
        &[],
        &[call(1, "project_run_headless", json!({"ticks": 1}))],
    );
    let result = result_of(&session, 0);
    assert_eq!(result["ok"], false, "{result}\n{}", session.stderr);
    assert_eq!(result["error"]["code"], "output_too_large", "{result}");
    assert_eq!(
        result["error"]["details"]["limit_bytes"],
        8 * 1024 * 1024,
        "{result}"
    );
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.decode(text).unwrap()
}
