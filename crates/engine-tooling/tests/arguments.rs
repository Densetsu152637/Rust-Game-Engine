use engine_project::Project;
use engine_tooling::arguments::{parse_run, validate_actions};
use std::{ffi::OsString, fs};

fn parse(values: &[&str]) -> engine_project::Result<engine_tooling::arguments::RunArguments> {
    parse_run(&values.iter().map(OsString::from).collect::<Vec<_>>())
}
fn error_code(values: &[&str]) -> String {
    parse(values).unwrap_err().diagnostics[0].code.clone()
}

#[test]
fn defaults_are_a_bounded_visible_run() {
    let args = parse(&["project.json"]).unwrap();
    assert!(!args.headless);
    assert_eq!(args.ticks, 120);
    assert!(args.inputs.is_empty());
    assert!(args.user_data.is_none() && args.scene_override.is_none());
}

#[test]
fn missing_manifest_is_an_argument_error() {
    assert_eq!(error_code(&[]), "command.arguments");
}

#[test]
fn ticks_must_be_numeric_and_within_bounds() {
    for ticks in ["abc", "-1", "0", "1000001", ""] {
        assert_eq!(
            error_code(&["p.json", "--ticks", ticks]),
            "command.arguments"
        );
    }
    assert_eq!(
        parse(&["p.json", "--ticks", "1000000"]).unwrap().ticks,
        1_000_000
    );
}

#[test]
fn malformed_inputs_are_rejected() {
    for input in ["noseparator", ":3", "jump:", "jump:x", "jump:-1"] {
        assert!(
            parse(&["p.json", "--ticks", "5", "--input", input]).is_err(),
            "{input}"
        );
    }
}

#[test]
fn options_require_values_and_may_not_repeat() {
    assert!(parse(&["p.json", "--ticks"]).is_err());
    assert!(parse(&["p.json", "--user-data"]).is_err());
    assert!(parse(&["p.json", "--ticks", "2", "--ticks", "3"]).is_err());
    assert!(parse(&["p.json", "--user-data", "a", "--user-data", "b"]).is_err());
}

#[test]
fn only_json_output_format_is_accepted() {
    assert!(parse(&["p.json", "--format", "json"]).is_ok());
    assert!(parse(&["p.json", "--format", "text"]).is_err());
    assert!(parse(&["p.json", "--format", "json", "--format", "json"]).is_err());
}

#[test]
fn inputs_group_by_tick_and_merge_duplicates() {
    let args = parse(&[
        "p.json", "--ticks", "4", "--input", "jump:2", "--input", "left:0", "--input", "jump:2",
        "--input", "left:3",
    ])
    .unwrap();
    assert_eq!(args.inputs.len(), 3);
    assert_eq!(args.inputs[&2].len(), 1);
    assert!(args.inputs[&0].contains("left") && args.inputs[&3].contains("left"));
    // The input bound is checked after --ticks regardless of argument order.
    assert!(parse(&["p.json", "--input", "jump:3", "--ticks", "4"]).is_ok());
}

#[test]
fn user_data_and_scene_override_paths_are_preserved() {
    let args = parse(&[
        "p.json",
        "--user-data",
        "saves dir",
        "--scene-override",
        "snapshot.json",
    ])
    .unwrap();
    assert_eq!(args.user_data.unwrap(), std::path::Path::new("saves dir"));
    assert_eq!(
        args.scene_override.unwrap(),
        std::path::Path::new("snapshot.json")
    );
}

#[test]
fn screenshot_options_default_to_640x360_and_accept_a_size() {
    let plain = parse(&["p.json"]).unwrap();
    assert!(plain.screenshot.is_none());
    assert_eq!(plain.screenshot_size, [640, 360]);
    let shot = parse(&["p.json", "--screenshot", "out dir/a.png"]).unwrap();
    assert_eq!(
        shot.screenshot.unwrap(),
        std::path::Path::new("out dir/a.png")
    );
    assert_eq!(shot.screenshot_size, [640, 360]);
    let sized = parse(&[
        "p.json",
        "--screenshot-size",
        "320x180",
        "--screenshot",
        "a.png",
        "--headless",
    ])
    .unwrap();
    assert_eq!(sized.screenshot_size, [320, 180]);
    assert!(sized.headless);
    assert_eq!(
        parse(&[
            "p.json",
            "--screenshot",
            "a.png",
            "--screenshot-size",
            "4096x1"
        ])
        .unwrap()
        .screenshot_size,
        [4096, 1]
    );
}

#[test]
fn invalid_screenshot_sizes_and_orphan_sizes_are_argument_errors() {
    for size in [
        "0x10", "10x0", "4097x10", "10", "axb", "10x", "x10", "10x10x10", "-1x5", "",
    ] {
        assert_eq!(
            error_code(&["p.json", "--screenshot", "a.png", "--screenshot-size", size]),
            "command.arguments",
            "{size}"
        );
    }
    assert_eq!(
        error_code(&["p.json", "--screenshot-size", "10x10"]),
        "command.arguments"
    );
    assert!(parse(&["p.json", "--screenshot"]).is_err());
    assert!(parse(&["p.json", "--screenshot", "a", "--screenshot", "b"]).is_err());
}

#[test]
fn injected_actions_must_be_declared_by_the_project() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("project.json"),
        r#"{"schema":1,"name":"inputs","startup_scene":"main.scene.json","assets":[],"inputActions":{"jump":"Space"},"inputBindings":{"ui:confirm":["Key:Enter"]}}"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
    )
    .unwrap();
    let project = Project::load(temp.path().join("project.json")).unwrap();
    let declared = parse(&[
        "p.json",
        "--ticks",
        "2",
        "--input",
        "jump:0",
        "--input",
        "ui:confirm:1",
    ])
    .unwrap();
    validate_actions(&project, &declared).unwrap();
    let unknown = parse(&["p.json", "--ticks", "2", "--input", "fly:0"]).unwrap();
    assert_eq!(
        validate_actions(&project, &unknown)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "command.input.unknown"
    );
}
