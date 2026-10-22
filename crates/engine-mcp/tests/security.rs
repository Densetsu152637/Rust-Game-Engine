//! Security regressions found in review: hidden and protected directories judged
//! on the canonical on-disk path (case variants, Windows 8.3 short names),
//! Windows device names, host-path scrubbing of results, glob length and file
//! modes. Each test fails against the code before the fix.

mod common;

use common::*;
use engine_mcp::{
    AccessLevel, Annotations, Content, ServerConfig, Tool, ToolContext, ToolDefinition,
    ToolFailure, ToolOutcome, ToolProvider, ToolResult,
};
use serde_json::{Map, Value, json};
use std::{fs, path::Path};

#[cfg(unix)]
const START: &str = "scenes/start.json";

fn put(root: &Path, path: &str, content: &str) -> Value {
    write(
        root,
        "file_write",
        json!({"path": path, "content": content}),
    )
}

fn resource(root: &Path, uri: &str) -> Value {
    exchange(
        root,
        read_only(),
        &[modern(1, "resources/read", json!({"uri": uri}))],
    )
    .remove(0)
}

fn bytes(root: &Path, relative: &str) -> Vec<u8> {
    fs::read(root.join(relative)).unwrap()
}

/// Creates `.git` and `.derived` in `root` and reports whether this volume
/// resolves their 8.3 short names (`GIT~1`, `DERIVE~1`). Short names depend on
/// the volume (some have them disabled), so the 8.3 tests skip with a note.
fn short_names_enabled(root: &Path) -> bool {
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::create_dir_all(root.join(".derived/native")).unwrap();
    let enabled = cfg!(windows)
        && fs::metadata(root.join("GIT~1")).is_ok_and(|m| m.is_dir())
        && fs::metadata(root.join("DERIVE~1")).is_ok_and(|m| m.is_dir());
    if !enabled {
        eprintln!("SKIPPED: this volume has no 8.3 short names for .git/.derived");
    }
    enabled
}

// ---- H1: 8.3 short names --------------------------------------------------------------------

#[test]
fn short_names_cannot_write_into_git_or_derived() {
    let dir = fixture();
    let root = dir.path();
    if !short_names_enabled(root) {
        return;
    }
    let hook = put(root, "GIT~1/hooks/pre-commit", "#!/bin/sh\n");
    assert_eq!(error_code(&hook), "path_protected", "{hook}");
    assert!(!root.join(".git/hooks/pre-commit").exists());
    let derived = put(root, "DERIVE~1/x.txt", "x");
    assert_eq!(error_code(&derived), "path_protected", "{derived}");
    assert!(!root.join(".derived/x.txt").exists());
    // An existing file behind the alias is protected too.
    fs::write(root.join(".git/config"), "[core]\n").unwrap();
    let existing = put(root, "GIT~1/config", "[evil]\n");
    assert_eq!(error_code(&existing), "path_protected", "{existing}");
    assert_eq!(bytes(root, ".git/config"), b"[core]\n");
}

#[test]
fn short_names_cannot_read_list_or_search_git_and_derived() {
    let dir = fixture();
    let root = dir.path();
    if !short_names_enabled(root) {
        return;
    }
    fs::write(root.join(".git/config"), "[core]\nsecret = 1\n").unwrap();
    fs::write(root.join(".derived/native/x.txt"), "derived secret").unwrap();
    for (tool, args) in [
        ("file_read", json!({"path": "GIT~1/config"})),
        ("file_read", json!({"path": "DERIVE~1/native/x.txt"})),
        ("file_list", json!({"path": "GIT~1"})),
        ("file_list", json!({"path": "DERIVE~1", "recursive": true})),
        ("search_text", json!({"query": "secret", "path": "GIT~1"})),
    ] {
        let response = data(root, tool, args.clone());
        assert_eq!(error_code(&response), "path_invalid", "{tool} {args}");
    }
    for uri in ["project://GIT~1/config", "project://DERIVE~1/native/x.txt"] {
        let response = resource(root, uri);
        assert_eq!(response["error"]["code"], -32002, "{uri}: {response}");
    }
}

// ---- M1: target/ and .derived/ are hidden, in any case -------------------------------------

#[test]
fn hidden_directories_are_not_served_in_any_case() {
    let dir = fixture();
    let root = dir.path();
    for hidden in ["target", ".derived", ".git"] {
        fs::create_dir_all(root.join(hidden).join("deep")).unwrap();
        fs::write(root.join(hidden).join("deep/secret.txt"), "top secret").unwrap();
    }
    let spellings = [
        "target/deep/secret.txt",
        "Target/deep/secret.txt",
        "TARGET/deep/secret.txt",
        ".derived/deep/secret.txt",
        ".Derived/deep/secret.txt",
        ".DERIVED/deep/secret.txt",
        ".git/deep/secret.txt",
        ".GIT/deep/secret.txt",
        "src/../target/deep/secret.txt",
    ];
    for path in spellings {
        let read = data(root, "file_read", json!({"path": path}));
        assert_eq!(error_code(&read), "path_invalid", "file_read {path}");
        let response = resource(root, &format!("project://{path}"));
        assert_eq!(response["error"]["code"], -32002, "resource {path}");
    }
    for directory in ["target", "Target/deep", ".derived", ".DERIVED", ".git"] {
        for (tool, args) in [
            ("file_list", json!({"path": directory})),
            ("search_text", json!({"query": "secret", "path": directory})),
        ] {
            let response = data(root, tool, args);
            assert_eq!(error_code(&response), "path_invalid", "{tool} {directory}");
        }
    }
    // Nothing from a hidden directory leaks into a project-wide search or listing.
    let search = data(root, "search_text", json!({"query": "top secret"}));
    assert_eq!(search["match_count"], 0, "{search}");
    let listing = exchange(root, read_only(), &[modern(1, "resources/list", json!({}))]);
    assert!(!listing[0].to_string().contains("secret.txt"));
}

#[test]
fn hidden_directories_refuse_writes_in_any_case_and_at_any_depth() {
    let dir = fixture();
    let root = dir.path();
    for path in [
        "target/new.txt",
        "Target/new.txt",
        "TARGET/deep/new.txt",
        ".derived/new.txt",
        ".DERIVED/native/new.txt",
        "src/target/new.rs",
        "notes/.Derived/new.txt",
    ] {
        let response = put(root, path, "x");
        assert_eq!(
            error_code(&response),
            "path_protected",
            "{path}: {response}"
        );
    }
    for path in [".git/hooks/pre-commit", ".GIT/config", "x/.Git/y"] {
        let response = put(root, path, "x");
        assert_eq!(error_code(&response), "path_invalid", "{path}: {response}");
    }
    assert!(!root.join("target").exists());
    assert!(!root.join(".derived").exists());
    assert!(!root.join(".git").exists());
    assert!(!root.join("src/target").exists());
}

// ---- M2: validation is decided on the canonical file ---------------------------------------

#[test]
fn a_case_variant_of_the_manifest_is_validated_and_rolled_back() {
    let dir = fixture();
    let root = dir.path();
    if !root.join("PROJECT.JSON").exists() {
        eprintln!("SKIPPED: this filesystem is case-sensitive");
        return;
    }
    let original = bytes(root, "project.json");
    let hash = data(root, "file_read", json!({"path": "project.json"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["startup_scene"] = json!("scenes/missing.json");
    let response = write(
        root,
        "file_write",
        json!({"path": "PROJECT.JSON", "content": manifest.to_string(), "expected_hash": hash}),
    );
    assert_eq!(error_code(&response), "project.asset.missing", "{response}");
    assert_eq!(response["error"]["details"]["rolled_back"], true);
    assert_eq!(bytes(root, "project.json"), original, "manifest restored");
    assert_eq!(data(root, "project_validate", json!({}))["ok"], true);
}

#[test]
fn a_case_variant_of_a_scene_is_validated_before_it_is_written() {
    let dir = fixture();
    let root = dir.path();
    if !root.join("SCENES/START.JSON").exists() {
        eprintln!("SKIPPED: this filesystem is case-sensitive");
        return;
    }
    let original = bytes(root, "scenes/start.json");
    let hash = data(root, "file_read", json!({"path": "scenes/start.json"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = write(
        root,
        "file_write",
        json!({"path": "SCENES/START.JSON", "content": "{\"not\":\"a scene\"}",
            "expected_hash": hash}),
    );
    assert_eq!(error_code(&response), "scene_invalid", "{response}");
    assert_eq!(bytes(root, "scenes/start.json"), original);
    // The reported path is the real on-disk spelling, not the requested one.
    let valid = write(
        root,
        "file_write",
        json!({"path": "SCENES/START.JSON", "content": String::from_utf8(original).unwrap(),
            "expected_hash": hash}),
    );
    assert_eq!(valid["path"], "scenes/start.json", "{valid}");
    assert_eq!(valid["validated"], "scene");
}

#[test]
fn a_short_name_of_the_manifest_is_validated_and_rolled_back() {
    let dir = fixture();
    let root = dir.path();
    let Some(alias) = ["PROJECT.JSO", "PROJEC~1.JSO"]
        .into_iter()
        .find(|alias| cfg!(windows) && root.join(alias).is_file())
    else {
        eprintln!("SKIPPED: no 8.3 short name for project.json on this volume");
        return;
    };
    let original = bytes(root, "project.json");
    let hash = data(root, "file_read", json!({"path": "project.json"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut manifest: Value = serde_json::from_slice(&original).unwrap();
    manifest["startup_scene"] = json!("scenes/missing.json");
    let response = write(
        root,
        "file_write",
        json!({"path": alias, "content": manifest.to_string(), "expected_hash": hash}),
    );
    assert_eq!(error_code(&response), "project.asset.missing", "{response}");
    assert_eq!(bytes(root, "project.json"), original);
}

// ---- M3: Windows device names ---------------------------------------------------------------

#[test]
fn windows_device_names_are_refused_everywhere() {
    let dir = fixture();
    let root = dir.path();
    for path in [
        "NUL",
        "nul",
        "CON",
        "prn",
        "AUX",
        "COM1",
        "com9",
        "LPT1",
        "lpt9",
        "aux.txt",
        "NUL.txt",
        "LPT1.txt",
        "sub/con.log",
        "notes/Prn.md",
        "x/LPT3.tar.gz",
        "nul .txt",
        "COM\u{b9}",
        "LPT\u{b2}.txt",
        "CONIN$",
        "trailing.",
        "trailing ",
    ] {
        let response = put(root, path, "x");
        assert_eq!(error_code(&response), "path_invalid", "{path}: {response}");
        let read = data(root, "file_read", json!({"path": path}));
        assert_eq!(error_code(&read), "path_invalid", "read {path}");
    }
    assert!(!root.join("sub").exists(), "no directory was created");
    // Look-alikes are ordinary names.
    for path in [
        "console.txt",
        "com10.txt",
        "nullable.rs",
        "aux_data.txt",
        "COM0",
    ] {
        let response = put(root, path, "x");
        assert_eq!(response["ok"], true, "{path}: {response}");
    }
}

// ---- M5: host paths never reach the client --------------------------------------------------

struct Leaky;

const FOREIGN: &str =
    r"C:\Users\Someone\.cargo\registry\src\index.crates.io-1\serde-1.0.0\src\lib.rs";

impl Tool for Leaky {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "leaky".into(),
            title: "Leaky".into(),
            description: "Test tool that echoes host paths in different places.".into(),
            input_schema: engine_mcp::schema::object(
                vec![(
                    "mode",
                    engine_mcp::schema::string("success, failure or verbatim"),
                )],
                &["mode"],
            ),
            output_schema: None,
            annotations: Annotations::READ_ONLY,
            access: AccessLevel::Read,
        }
    }

    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
        let verbatim_root = cx.root.to_string_lossy().into_owned();
        let plain_root = verbatim_root.trim_start_matches(r"\\?\").to_owned();
        let file = format!(r"{plain_root}\src\lib.rs");
        match args["mode"].as_str().unwrap() {
            "success" => Ok(ToolResult::new(json!({
                "warnings": [{"file": file, "rendered": format!("  --> {file}:3:9\n   |\n"),
                    "debug": format!("{file:?}"), "verbatim_prefixed": format!(r"{verbatim_root}\src\lib.rs")}],
                "cargo": FOREIGN, "root": plain_root,
            }))
            .with_content(Content::Text(format!("also {file}")))),
            "failure" => Err(ToolFailure::new("boom", format!("failed in {file}"))
                .with_details(json!({"stderr_tail": format!(
                    "error: could not compile\n --> {file}:3:9\n  = note: {FOREIGN}\nroot {plain_root}"
                )}))),
            _ => Ok(ToolResult::new(json!({"content": format!("path = {file}")})).verbatim()),
        }
    }
}

struct LeakyProvider;
impl ToolProvider for LeakyProvider {
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![Box::new(Leaky)]
    }
}

fn leaky(root: &Path, mode: &str) -> Value {
    result_with(
        root,
        ServerConfig::new().with_provider(LeakyProvider),
        "leaky",
        json!({"mode": mode}),
    )
}

/// Every spelling of the root that must not survive in a result.
fn root_spellings(root: &Path) -> Vec<String> {
    let verbatim = root.to_string_lossy().into_owned();
    let plain = verbatim.trim_start_matches(r"\\?\").to_owned();
    let mut all = vec![verbatim.clone(), plain.clone(), plain.replace('\\', "/")];
    all.push(plain.replace('\\', r"\\"));
    all.push(verbatim.replace('\\', r"\\"));
    all.retain(|s| !s.is_empty());
    all
}

#[test]
fn successful_results_never_contain_the_host_root_or_foreign_paths() {
    let dir = fixture();
    let root = dir.path().canonicalize().unwrap();
    let result = leaky(&root, "success");
    assert_eq!(result["isError"], false);
    let text = result.to_string();
    for spelling in root_spellings(&root) {
        assert!(!text.contains(&spelling), "{spelling} leaked: {text}");
    }
    assert!(!text.contains("Someone"), "{text}");
    assert!(!text.contains(".cargo"), "{text}");
    let data = &result["structuredContent"];
    assert_eq!(data["warnings"][0]["file"], "src/lib.rs");
    assert!(
        data["warnings"][0]["rendered"]
            .as_str()
            .unwrap()
            .contains("--> src/lib.rs:3:9"),
        "{data}"
    );
    assert_eq!(data["warnings"][0]["verbatim_prefixed"], "src/lib.rs");
    assert_eq!(data["cargo"], "<host>");
    assert_eq!(data["root"], ".");
    // The text blocks are scrubbed too.
    assert_eq!(result["content"][1]["text"], "also src/lib.rs");
}

#[test]
fn failures_scrub_details_and_the_cargo_home_as_well() {
    let dir = fixture();
    let root = dir.path().canonicalize().unwrap();
    let result = leaky(&root, "failure");
    assert_eq!(result["isError"], true);
    let text = result.to_string();
    for spelling in root_spellings(&root) {
        assert!(!text.contains(&spelling), "{spelling} leaked: {text}");
    }
    assert!(!text.contains("Someone"), "cargo home leaked: {text}");
    let tail = result["structuredContent"]["error"]["details"]["stderr_tail"]
        .as_str()
        .unwrap();
    assert!(tail.contains("--> src/lib.rs:3:9"), "{tail}");
    assert!(tail.contains("= note: <host>"), "{tail}");
    assert!(tail.ends_with("root ."), "{tail}");
}

#[test]
fn verbatim_results_and_file_content_are_not_rewritten() {
    let dir = fixture();
    let root = dir.path().canonicalize().unwrap();
    // A tool that marks its payload as project content keeps it unchanged...
    let result = leaky(&root, "verbatim");
    let content = result["structuredContent"]["content"].as_str().unwrap();
    assert!(content.contains("src\\lib.rs") || content.contains("src/lib.rs"));
    assert!(content.contains(root.to_string_lossy().trim_start_matches(r"\\?\")));
    // ...which is what file_read relies on: file text round-trips exactly.
    let text = format!(
        "path = \"{}\"\n/etc/passwd C:\\Windows\\win.ini\n",
        root.display()
    );
    fs::write(root.join("notes/paths.txt"), &text).unwrap();
    let read = data(&root, "file_read", json!({"path": "notes/paths.txt"}));
    assert_eq!(read["content"], text);
    let search = data(&root, "search_text", json!({"query": "passwd"}));
    assert_eq!(search["matches"][0]["text"], text.lines().nth(1).unwrap());
}

// ---- L1: glob length ------------------------------------------------------------------------

#[test]
fn globs_are_capped_at_512_bytes() {
    let dir = fixture();
    let root = dir.path();
    let long = "*".repeat(513);
    for (tool, args) in [
        ("file_list", json!({"glob": long})),
        ("search_text", json!({"query": "TODO", "glob": long})),
    ] {
        let response = data(root, tool, args);
        assert_eq!(
            error_code(&response),
            "invalid_argument",
            "{tool}: {response}"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("glob"),
            "{response}"
        );
    }
    let at_limit = format!("{}.rs", "?".repeat(509));
    assert_eq!(at_limit.len(), 512);
    for (tool, args) in [
        ("file_list", json!({"glob": at_limit})),
        ("search_text", json!({"query": "TODO", "glob": at_limit})),
    ] {
        assert_eq!(data(root, tool, args)["ok"], true, "{tool}");
    }
}

// ---- L2: scene edit tools use the same hidden/protected rules ------------------------------

#[test]
fn scene_tools_refuse_scenes_in_protected_directories() {
    let dir = fixture();
    let root = dir.path();
    let scene = bytes(root, "scenes/second.json");
    for hidden in ["target", ".derived"] {
        fs::create_dir_all(root.join(hidden)).unwrap();
        fs::write(root.join(hidden).join("fake.scene.json"), &scene).unwrap();
    }
    for path in [
        "target/fake.scene.json",
        "Target/fake.scene.json",
        ".derived/fake.scene.json",
    ] {
        let calls = [
            ("scene_inspect", json!({"scene": path})),
            ("entity_get", json!({"scene": path, "id": "npc"})),
            (
                "entity_create",
                json!({"scene": path, "expected_revision": "x", "id": "new"}),
            ),
            (
                "entity_update",
                json!({"scene": path, "expected_revision": "x", "id": "npc", "name": "n"}),
            ),
            (
                "entity_delete",
                json!({"scene": path, "expected_revision": "x", "id": "npc"}),
            ),
            (
                "component_set",
                json!({"scene": path, "expected_revision": "x", "entity": "npc",
                    "component": "Transform", "value": {"position": [1, 1, 1]}}),
            ),
            (
                "component_remove",
                json!({"scene": path, "expected_revision": "x", "entity": "npc",
                    "component": "Transform"}),
            ),
        ];
        for (tool, args) in calls {
            let response = write(root, tool, args);
            assert_eq!(
                error_code(&response),
                "path_invalid",
                "{tool} {path}: {response}"
            );
        }
    }
    assert_eq!(bytes(root, "target/fake.scene.json"), scene);
    assert_eq!(bytes(root, ".derived/fake.scene.json"), scene);
}

// ---- L3: file modes -------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn overwriting_keeps_the_original_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    let root = dir.path();
    let script = root.join("notes/run.sh");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let hash = data(root, "file_read", json!({"path": "notes/run.sh"}))["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = write(
        root,
        "file_write",
        json!({"path": "notes/run.sh", "content": "#!/bin/sh\necho hi\n", "expected_hash": hash}),
    );
    assert_eq!(response["ok"], true, "{response}");
    let mode = fs::metadata(&script).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "mode was {mode:o}");
}

#[cfg(unix)]
#[test]
fn scene_edits_keep_the_scene_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    let root = dir.path();
    let scene = root.join(START);
    fs::set_permissions(&scene, fs::Permissions::from_mode(0o664)).unwrap();
    let edit = write(
        root,
        "entity_create",
        json!({"scene": START, "expected_revision": revision(root, START), "id": "extra"}),
    );
    assert_eq!(edit["ok"], true, "{edit}");
    let mode = fs::metadata(&scene).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o664, "mode was {mode:o}");
}
