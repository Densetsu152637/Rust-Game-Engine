//! Behavioural tests of the `engine` binary's JSON contract. None of these
//! compile external Rust; see `runtime_pipeline.rs` for real generations.
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const SHADER: &str = "@vertex fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> { return vec4<f32>(f32(i), 0.0, 0.0, 1.0); }\n";

struct Output {
    code: i32,
    json: Value,
}

fn engine<S: AsRef<OsStr>>(args: &[S], cwd: &Path) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "exactly one JSON result line: {stdout}");
    Output {
        code: output.status.code().unwrap(),
        json: serde_json::from_str(lines[0]).unwrap(),
    }
}

fn failure_code(output: &Output) -> &str {
    assert_eq!(output.json["ok"], false, "{}", output.json);
    assert_eq!(output.json["schema"], 1);
    output.json["result"]["diagnostics"][0]["code"]
        .as_str()
        .unwrap()
}

/// Content-only project: texture, shader, startup scene and a secondary scene.
fn project(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("project.json"),
        r#"{"schema":1,"name":"cli fixture","startup_scene":"main.scene.json","assets":[
        {"id":"asset:color","kind":"texture","path":"color.ppm"},
        {"id":"asset:shader","kind":"shader","path":"shaders/basic.wgsl"},
        {"id":"asset:level","kind":"scene","path":"level.scene.json"}],
        "inputActions":{"jump":"Space"}}"#,
    )
    .unwrap();
    fs::write(
        root.join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[{"id":"entity:a","name":"A","components":{"Transform":{"position":[1,2,3]}}}]}"#,
    )
    .unwrap();
    fs::write(
        root.join("level.scene.json"),
        r#"{"schema":1,"scene_id":"scene:level","entities":[]}"#,
    )
    .unwrap();
    fs::write(root.join("color.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    fs::create_dir_all(root.join("shaders")).unwrap();
    fs::write(root.join("shaders/basic.wgsl"), SHADER).unwrap();
    root.join("project.json")
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let manifest = project(&temp.path().join("game"));
    (temp, manifest)
}

#[test]
fn no_arguments_report_usage_as_json() {
    let temp = tempfile::tempdir().unwrap();
    let output = engine::<&str>(&[], temp.path());
    assert_eq!(output.code, 2);
    assert_eq!(failure_code(&output), "command.arguments");
    assert_eq!(output.json["command"], "");
}

#[test]
fn unknown_commands_are_argument_errors() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["frobnicate"],
        vec!["project", "frobnicate", "x"],
        vec!["project"],
        vec!["scene", "list"],
    ] {
        let output = engine(&args, temp.path());
        assert_eq!(output.code, 2, "{args:?}");
        assert_eq!(failure_code(&output), "command.arguments");
    }
    let output = engine(&["project", "frobnicate"], temp.path());
    assert_eq!(output.json["command"], "project frobnicate");
}

#[test]
fn command_arity_is_checked() {
    let (temp, manifest) = fixture();
    for args in [
        vec![OsStr::new("project"), "validate".as_ref()],
        vec![
            "project".as_ref(),
            "validate".as_ref(),
            manifest.as_os_str(),
            manifest.as_os_str(),
        ],
        vec!["project".as_ref(), "init".as_ref(), manifest.as_os_str()],
        vec!["project".as_ref(), "assets".as_ref()],
        vec!["project".as_ref(), "package".as_ref(), manifest.as_os_str()],
    ] {
        let output = engine(&args, temp.path());
        assert_eq!(output.code, 2, "{args:?}");
        assert_eq!(failure_code(&output), "command.arguments");
    }
}

#[test]
fn format_option_accepts_only_one_json() {
    let (temp, manifest) = fixture();
    let ok = engine(
        &[
            "project".as_ref(),
            "validate".as_ref(),
            "--format".as_ref(),
            "json".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(ok.code, 0);
    for extra in [
        vec!["--format", "xml"],
        vec!["--format", "json", "--format", "json"],
        vec!["--format"],
        vec!["--verbose"],
    ] {
        let mut args = vec![
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ];
        args.extend(extra.iter().map(OsStr::new));
        let output = engine(&args, temp.path());
        assert_eq!(output.code, 2, "{extra:?}");
        assert_eq!(failure_code(&output), "command.arguments");
    }
}

#[test]
fn validate_reports_project_scenes_and_skips_rust_for_content_projects() {
    let (temp, manifest) = fixture();
    let output = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 0, "{}", output.json);
    let result = &output.json["result"];
    assert_eq!(output.json["command"], "project validate");
    assert_eq!(result["project"], "cli fixture");
    assert_eq!(result["assets"], 3);
    assert_eq!(result["scenes"].as_array().unwrap().len(), 2);
    assert_eq!(result["scenes"][0]["id"], "scene:main");
    assert!(result["rust"].is_null());
    assert_eq!(result["diagnostics"], serde_json::json!([]));
}

#[test]
fn validate_accepts_a_manifest_relative_to_the_working_directory() {
    let (temp, _) = fixture();
    let output = engine(
        &["project", "validate", "game/project.json", "--no-rust"],
        temp.path(),
    );
    assert_eq!(output.code, 0, "{}", output.json);
    assert!(output.json["result"]["rust"].is_null());
}

#[test]
fn validate_missing_manifest_is_a_project_failure() {
    let temp = tempfile::tempdir().unwrap();
    let output = engine(&["project", "validate", "absent.json"], temp.path());
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "project.file.io");
}

#[test]
fn validate_rejects_malformed_and_unsupported_manifests() {
    let (temp, manifest) = fixture();
    fs::write(&manifest, "{ not json").unwrap();
    let output = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "project.document.malformed");
    fs::write(
        &manifest,
        r#"{"schema":2,"name":"x","startup_scene":"main.scene.json","assets":[]}"#,
    )
    .unwrap();
    let output = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(failure_code(&output), "project.schema.unsupported");
}

#[test]
fn validate_reports_missing_asset_files_with_location() {
    let (temp, manifest) = fixture();
    fs::remove_file(manifest.parent().unwrap().join("color.ppm")).unwrap();
    let output = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "project.asset.missing");
    assert!(
        output.json["result"]["diagnostics"][0]["file"]
            .as_str()
            .unwrap()
            .contains("color.ppm")
    );
}

#[test]
fn validate_checks_every_declared_scene_not_only_startup() {
    let (temp, manifest) = fixture();
    fs::write(
        manifest.parent().unwrap().join("level.scene.json"),
        r#"{"schema":1,"scene_id":"scene:level","entities":[{"id":"entity:p","name":"P","components":{"Script":{"asset":"asset:absent"}}}]}"#,
    )
    .unwrap();
    let output = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "project.asset.unknown");
}

#[test]
fn script_assets_without_a_rust_crate_are_reported() {
    let (temp, manifest) = fixture();
    let root = manifest.parent().unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn configure() {}\n").unwrap();
    fs::write(&manifest, r#"{"schema":1,"name":"scripted","startup_scene":"main.scene.json","assets":[{"id":"asset:player","kind":"script","path":"src/lib.rs"}],"inputActions":{"jump":"Space"}}"#).unwrap();
    for command in ["validate", "package"] {
        let mut args = vec![
            OsStr::new("project"),
            command.as_ref(),
            manifest.as_os_str(),
        ];
        let destination = temp.path().join("out");
        if command == "package" {
            args.push(destination.as_os_str());
        }
        let output = engine(&args, temp.path());
        assert_eq!(output.code, 1, "{command}");
        assert_eq!(failure_code(&output), "project.rust.missing");
    }
    let output = engine(
        &[
            OsStr::new("run"),
            manifest.as_os_str(),
            "--headless".as_ref(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "project.rust.missing");
}

#[test]
fn asset_index_is_written_atomically_and_stable() {
    let (temp, manifest) = fixture();
    let args = [
        OsStr::new("project"),
        "assets".as_ref(),
        manifest.as_os_str(),
    ];
    let first = engine(&args, temp.path());
    assert_eq!(first.code, 0, "{}", first.json);
    let assets = first.json["result"]["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 3);
    assert!(
        assets
            .iter()
            .all(|a| a["content_hash"].as_str().unwrap().len() == 64)
    );
    let index = PathBuf::from(first.json["result"]["index"].as_str().unwrap());
    assert!(index.ends_with(".derived/assets.json"));
    let saved: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    assert_eq!(saved["assets"], first.json["result"]["assets"]);
    let second = engine(&args, temp.path());
    assert_eq!(first.json["result"], second.json["result"]);
}

#[test]
fn shader_import_publishes_content_hashed_sources() {
    let (temp, manifest) = fixture();
    let output = engine(
        &[
            OsStr::new("project"),
            "shaders".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 0, "{}", output.json);
    let result = &output.json["result"];
    let directory = PathBuf::from(result["directory"].as_str().unwrap());
    let shader = &result["shaders"][0];
    assert_eq!(shader["id"], "asset:shader");
    let path = directory.join(shader["path"].as_str().unwrap());
    assert_eq!(fs::read_to_string(path).unwrap(), SHADER);
    assert!(directory.join("index.json").is_file());
}

#[test]
fn invalid_shader_fails_and_keeps_previous_index() {
    let (temp, manifest) = fixture();
    let args = [
        OsStr::new("project"),
        "shaders".as_ref(),
        manifest.as_os_str(),
    ];
    let first = engine(&args, temp.path());
    let index =
        PathBuf::from(first.json["result"]["directory"].as_str().unwrap()).join("index.json");
    let before = fs::read(&index).unwrap();
    fs::write(
        manifest.parent().unwrap().join("shaders/basic.wgsl"),
        "@vertex fn broken(",
    )
    .unwrap();
    let failed = engine(&args, temp.path());
    assert_eq!(failed.code, 1);
    assert_eq!(failure_code(&failed), "shader.invalid");
    assert!(
        failed.json["result"]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("WGSL")
    );
    assert_eq!(before, fs::read(index).unwrap());
}

#[test]
fn init_copies_a_valid_template_without_generated_output() {
    let (temp, manifest) = fixture();
    engine(
        &[
            OsStr::new("project"),
            "assets".as_ref(),
            manifest.as_os_str(),
        ],
        temp.path(),
    );
    let destination = temp.path().join("copy");
    let output = engine(
        &[
            OsStr::new("project"),
            "init".as_ref(),
            manifest.as_os_str(),
            destination.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 0, "{}", output.json);
    let created = PathBuf::from(output.json["result"]["manifest"].as_str().unwrap());
    assert_eq!(created, destination.join("project.json"));
    assert!(!destination.join(".derived").exists());
    let validated = engine(
        &[
            OsStr::new("project"),
            "validate".as_ref(),
            created.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(validated.code, 0);
}

#[test]
fn init_refuses_existing_and_source_overlapping_destinations() {
    let (temp, manifest) = fixture();
    let existing = temp.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep.txt"), "user data").unwrap();
    let output = engine(
        &[
            OsStr::new("project"),
            "init".as_ref(),
            manifest.as_os_str(),
            existing.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1);
    assert_eq!(failure_code(&output), "tooling.destination.exists");
    assert_eq!(
        fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "user data"
    );
    let inside = manifest.parent().unwrap().join("nested");
    let output = engine(
        &[
            OsStr::new("project"),
            "init".as_ref(),
            manifest.as_os_str(),
            inside.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(failure_code(&output), "tooling.destination.source");
    assert!(!inside.exists());
}

#[test]
fn package_destination_is_checked_before_compilation() {
    let (temp, manifest) = fixture();
    let root = manifest.parent().unwrap();
    let cases = [
        (root.join("exports"), "tooling.destination.source"),
        (root.to_path_buf(), "tooling.destination.source"),
        (temp.path().to_path_buf(), "tooling.destination.exists"),
        // Generated dist/ is allowed; the content-only project then lacks a crate.
        (root.join("dist/out"), "project.rust.missing"),
    ];
    if !root.join("dist").exists() {
        fs::create_dir(root.join("dist")).unwrap();
    }
    for (destination, code) in cases {
        let output = engine(
            &[
                OsStr::new("project"),
                "package".as_ref(),
                manifest.as_os_str(),
                destination.as_os_str(),
            ],
            temp.path(),
        );
        assert_eq!(output.code, 1, "{}", destination.display());
        assert_eq!(failure_code(&output), code, "{}", destination.display());
    }
    assert!(!root.join(".derived/native").exists());
}

#[test]
fn package_rejects_unknown_profiles() {
    let (temp, manifest) = fixture();
    let output = engine(
        &[
            OsStr::new("project"),
            "package".as_ref(),
            manifest.as_os_str(),
            temp.path().join("out").as_os_str(),
            "--profile".as_ref(),
            "fastest".as_ref(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 2);
    assert_eq!(failure_code(&output), "command.arguments");
}

#[test]
fn scene_inspect_reports_scene_and_stable_revision() {
    let (temp, manifest) = fixture();
    let root = manifest.parent().unwrap();
    let args = [
        OsStr::new("scene"),
        "inspect".as_ref(),
        "main.scene.json".as_ref(),
        "--project-root".as_ref(),
        root.as_os_str(),
    ];
    let first = engine(&args, temp.path());
    assert_eq!(first.code, 0, "{}", first.json);
    assert_eq!(first.json["command"], "scene inspect");
    let result = &first.json["result"];
    assert_eq!(result["entities"], 1);
    assert_eq!(result["scene"]["entities"][0]["id"], "entity:a");
    assert_eq!(result["revision"].as_str().unwrap().len(), 64);
    let absolute = root.join("main.scene.json");
    let second = engine(
        &[
            OsStr::new("scene"),
            "inspect".as_ref(),
            absolute.as_os_str(),
            "--project-root".as_ref(),
            root.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(
        first.json["result"]["revision"],
        second.json["result"]["revision"]
    );
}

#[test]
fn scene_inspect_requires_project_root() {
    let (temp, _) = fixture();
    let output = engine(&["scene", "inspect", "main.scene.json"], temp.path());
    assert_eq!(output.code, 2);
    assert_eq!(failure_code(&output), "command.arguments");
}

#[test]
fn scene_inspect_is_confined_to_the_project_root() {
    let (temp, manifest) = fixture();
    let root = manifest.parent().unwrap();
    let outside = temp.path().join("outside.scene.json");
    fs::write(
        &outside,
        r#"{"schema":1,"scene_id":"scene:outside","entities":[]}"#,
    )
    .unwrap();
    for scene in [
        PathBuf::from("../outside.scene.json"),
        outside.clone(),
        PathBuf::from("missing.scene.json"),
    ] {
        let output = engine(
            &[
                OsStr::new("scene"),
                "inspect".as_ref(),
                scene.as_os_str(),
                "--project-root".as_ref(),
                root.as_os_str(),
            ],
            temp.path(),
        );
        assert_eq!(output.code, 1, "{}", scene.display());
        let code = failure_code(&output);
        assert!(
            code == "project.path.outside_root" || code == "project.asset.missing",
            "{code}"
        );
    }
}

#[test]
fn scene_inspect_reports_invalid_references() {
    let (temp, manifest) = fixture();
    let root = manifest.parent().unwrap();
    fs::write(
        root.join("broken.scene.json"),
        r#"{"schema":1,"scene_id":"scene:broken","entities":[{"id":"entity:x","name":"X","components":{"SpriteRenderer":{"texture":"asset:none","size":[1,1]}}}]}"#,
    )
    .unwrap();
    let output = engine(
        &[
            OsStr::new("scene"),
            "inspect".as_ref(),
            "broken.scene.json".as_ref(),
            "--project-root".as_ref(),
            root.as_os_str(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 1, "{}", output.json);
}

#[test]
fn run_rejects_undeclared_input_before_compiling() {
    let (temp, manifest) = fixture();
    let output = engine(
        &[
            OsStr::new("run"),
            manifest.as_os_str(),
            "--headless".as_ref(),
            "--ticks".as_ref(),
            "2".as_ref(),
            "--input".as_ref(),
            "fly:0".as_ref(),
        ],
        temp.path(),
    );
    assert_eq!(output.code, 2);
    assert_eq!(output.json["command"], "run");
    assert_eq!(failure_code(&output), "command.input.unknown");
    assert!(!manifest.parent().unwrap().join(".derived/native").exists());
}

#[test]
fn run_rejects_invalid_bounds() {
    let (temp, manifest) = fixture();
    for extra in [
        vec!["--ticks", "0"],
        vec!["--ticks", "2", "--input", "jump:2"],
    ] {
        let mut args = vec![OsStr::new("run"), manifest.as_os_str()];
        args.extend(extra.iter().map(OsStr::new));
        let output = engine(&args, temp.path());
        assert_eq!(output.code, 2, "{extra:?}");
        assert_eq!(failure_code(&output), "command.arguments");
    }
}

#[test]
fn mcp_and_editor_usage_errors_do_not_open_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let mcp = Command::new(env!("CARGO_BIN_EXE_engine"))
        .arg("mcp")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert_eq!(mcp.status.code(), Some(2));
    assert!(mcp.stdout.is_empty());
    let editor = engine(&["editor"], temp.path());
    assert_eq!(editor.code, 2);
    assert_eq!(failure_code(&editor), "command.arguments");
}

#[test]
fn mcp_serves_the_project_over_stdio() {
    let (_temp, manifest) = fixture();
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine"))
        .arg("mcp")
        .arg(manifest.parent().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"test","version":"1"}}}}}}"#
        )
        .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let response: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response["id"], 1);
    assert!(response["result"].is_object(), "{response}");
}
