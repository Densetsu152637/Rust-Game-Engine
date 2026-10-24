use serde_json::Value;
use std::process::Command;

fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_engine"))
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

#[test]
fn platform_query_reports_host_and_unsupported_mobile_web_targets() {
    let (code, value) = run(&["platform", "--format", "json"]);
    assert_eq!(code, 0);
    assert_eq!(value["command"], "platform");
    assert_eq!(
        value["result"]["host"]["architecture"],
        std::env::consts::ARCH
    );
    assert_eq!(value["result"]["platforms"].as_array().unwrap().len(), 7);
    for target in ["android", "ios", "web", "unknown"] {
        let (code, value) = run(&["platform", "--target", target]);
        assert_eq!(code, 0);
        let platforms = value["result"]["platforms"].as_array().unwrap();
        assert_eq!(platforms.len(), 1);
        assert_eq!(platforms[0]["platform"], target);
        assert!(
            platforms[0]["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["implemented"] == false)
        );
    }
}

#[test]
fn platform_query_rejects_unknown_targets_and_extra_arguments() {
    for args in [
        vec!["platform", "--target", "console"],
        vec!["platform", "extra"],
        vec!["platform", "--target"],
    ] {
        let (code, value) = run(&args);
        assert_eq!(code, 2);
        assert_eq!(value["ok"], false);
    }
}
