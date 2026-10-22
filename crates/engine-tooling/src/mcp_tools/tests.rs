//! Unit tests for the execution tools.

use super::*;
use super::{exec::*, run::*, support::*};
use crate::compiler::BuildFailure;
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, thread, time::Duration};

#[test]
fn a_missing_gpu_adapter_becomes_the_gpu_unavailable_error() {
    let envelope = json!({"schema":1,"ok":false,"command":"run","result":{"diagnostics":[
            {"code":"render.gpu.unavailable","severity":"error","file":"","path":"",
             "message":"No usable GPU adapter for an offscreen render: no compatible GPU adapter"}]}});
    let failure = run_failure(&envelope, Some(3));
    assert_eq!(failure.code, "gpu_unavailable");
    assert!(failure.message.contains("No usable GPU adapter"));
    assert!(
        failure
            .hint
            .as_deref()
            .unwrap()
            .contains("project_run_headless")
    );
    assert_eq!(failure.diagnostics.len(), 1);
    assert_eq!(failure.details.as_ref().unwrap()["exit_code"], 3);
}

#[test]
fn other_runtime_failures_keep_their_diagnostic_code() {
    let envelope = json!({"ok":false,"result":{"diagnostics":[
            {"code":"runtime.panic","severity":"error","file":"","path":"","message":"Native gameplay panicked"}]}});
    assert_eq!(run_failure(&envelope, Some(3)).code, "runtime.panic");
    assert_eq!(
        run_failure(&json!({"ok":false}), None).code,
        "runtime.failed"
    );
}

#[test]
fn compiler_diagnostics_are_ordered_bounded_and_counted() {
    let mut raw = Vec::new();
    for i in 0..40 {
        raw.push(json!({"level":"warning","message":format!("w{i}"),"spans":[],"rendered":"w"}));
    }
    raw.push(
        json!({"level":"error","message":"boom","code":{"code":"E0308"},
            "spans":[{"is_primary":true,"file_name":"src/lib.rs","line_start":3,"column_start":9}],
            "rendered":"x".repeat(5000)}),
    );
    raw.push(json!({"level":"note","message":"ignored"}));
    let (list, errors, warnings) = diagnostics(&raw);
    assert_eq!((errors, warnings), (1, 40));
    assert_eq!(list.len(), MAX_DIAGNOSTICS);
    assert_eq!(list[0]["code"], "rust.E0308", "errors come first");
    assert_eq!(list[0]["file"], "src/lib.rs");
    assert_eq!(list[0]["path"], "3:9");
    assert!(list[0]["rendered"].as_str().unwrap().chars().count() <= CLIP + 1);
    assert!(list.iter().all(|d| d["message"] != "ignored"));
}

#[test]
fn build_failures_describe_errors_timeouts_and_the_last_good_generation() {
    let error = BuildFailure {
        code: "compiler.build".into(),
        message: "cargo failed".into(),
        diagnostics: vec![json!({"level":"error","message":"bad","spans":[],"rendered":"r"})],
        stderr: "s".repeat(10_000),
        exit_code: Some(101),
        cancelled: false,
    };
    let failure = build_failure(&error, false, Some("gen-1".into()));
    assert_eq!(failure.code, "compiler.build");
    assert!(failure.message.contains("1 error"));
    assert!(failure.hint.as_deref().unwrap().contains("untouched"));
    let details = failure.details.as_ref().unwrap();
    assert_eq!(details["last_good_generation"], "gen-1");
    assert_eq!(details["stderr_tail"].as_str().unwrap().len(), 4000);
    assert_eq!(details["exit_code"], 101);
    let timeout = build_failure(&error, true, None);
    assert_eq!(timeout.code, "build_timeout");
    assert!(timeout.hint.as_deref().unwrap().contains("timeout_seconds"));
    assert_eq!(
        timeout.details.as_ref().unwrap()["last_good_generation"],
        Value::Null
    );
}

#[test]
fn run_summaries_are_bounded() {
    let entities: Vec<Value> = (0..300)
        .map(|i| json!({"id": format!("e{i:03}"), "name": "n", "components": {}}))
        .collect();
    let logs: Vec<String> = (0..500).map(|i| format!("log {i}")).collect();
    let report = json!({"ticks": 5, "fixed_delta": 0.5, "scene": {
            "scene": {"scene_id": "s", "entities": entities}, "logs": logs,
            "triggers": [], "live_entities": 300, "active_scripts": 0, "scene_revision": 2}});
    let summary = summarize(&report, None, 50);
    assert_eq!(summary["entities"].as_array().unwrap().len(), 50);
    assert_eq!(summary["entity_count"], 300);
    assert_eq!(summary["entities_truncated"], true);
    assert_eq!(summary["logs"].as_array().unwrap().len(), MAX_LOGS);
    assert_eq!(
        summary["logs"][MAX_LOGS - 1],
        "log 499",
        "the newest logs are kept"
    );
    assert_eq!(summary["log_count"], 500);
    assert_eq!(summary["logs_truncated"], true);
    let filtered = summarize(&report, Some(&["e007", "e299", "zzz"]), 50);
    let ids: Vec<_> = filtered["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["e007", "e299"]);
    assert_eq!(filtered["entities_truncated"], false);
    let huge: Vec<Value> = (0..10)
        .map(|i| json!({"id": format!("h{i}"), "blob": "x".repeat(60_000)}))
        .collect();
    let big = json!({"scene": {"scene": {"entities": huge}}});
    let capped = summarize(&big, None, 50);
    assert!(capped["entities"].as_array().unwrap().len() < 10);
    assert_eq!(capped["entities_truncated"], true);
}

#[test]
fn the_deadline_fires_and_reports_it() {
    let (value, fired) = with_deadline(Duration::from_millis(60), |cancel| {
        while !cancel.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(5));
        }
        7
    });
    assert_eq!((value, fired), (7, true));
    let (value, fired) = with_deadline(Duration::from_secs(30), |_| 1);
    assert_eq!((value, fired), (1, false));
}

#[test]
fn clip_and_tail_respect_char_boundaries() {
    assert_eq!(clip("héllo", 2), "h…");
    assert_eq!(clip("abc", 10), "abc");
    assert_eq!(tail("héllo", 3), "llo");
    assert_eq!(tail("ab", 10), "ab");
}
