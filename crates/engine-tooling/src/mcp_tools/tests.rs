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
fn run_telemetry_exposes_recent_profile_and_devices_with_explicit_bounds() {
    let report = json!({"profile": {
        "frames": (0..100).map(|tick| json!({"tick":tick})).collect::<Vec<_>>(),
        "frames_recorded": 100, "stages": []
    }, "scene": {
        "network":{"connected":true},
        "input_devices":{"devices":[{"descriptor":{"id":"switch"},"connected":true}]},
        "upscaler":{"requested":{"mode":"dlss_sr","quality":"quality"},"state":"not_rendered",
            "evaluated_frames":0,"history_resets":0},
        "frame_generation":{"requested":{"mode":"dlss_fg","multiplier":2},"state":"not_rendered",
            "generated_frames":0,"presented_frames":0,"history_resets":0},
        "localization":{"locale":"fr","default_locale":"en","generation":3,"pseudo":false,
            "locales":["en","fr"],"resolutions":12,"missing_messages":1},
        "render_pipeline":{"state":"not_rendered","overrides":{"engine.opaque":false},
            "features":[{"name":"engine.opaque","stage":"opaque","enabled":false,
                "source":"view_override","activity":"disabled"}],
            "unknown_overrides":[],"draws":0,"draw_calls":0,"instances":0},
        "acoustics":{"requested":"Auto","active":"Cpu","state":"Active","sources":[]},
        "behavior_trees":{"total":1,"dropped_events":0,"agents":[{"entity":"guard","graph":"patrol",
            "enabled":true,"ticks":12,"last_finished":"success","active_path":["root","seq"],
            "blackboard":{"alarm":true}}]},
        "state_trees":{"total":1,"dropped_events":0,"agents":[{"entity":"guard","graph":"mood",
            "enabled":true,"ticks":9,"phase":"running","last_finished":null,
            "active_path":["alert","chase"],
            "transitions":[{"tick":4,"event":"transition","id":"spot","to":"chase"}],
            "blackboard":{"alarm":true}}]}
    }});
    let summary = summarize(&report, None, 50);
    assert_eq!(summary["profile"]["frames"].as_array().unwrap().len(), 32);
    assert_eq!(summary["profile"]["frames"][0]["tick"], 68);
    assert_eq!(summary["profile"]["frames_recorded"], 100);
    assert_eq!(summary["profile_frames_truncated"], true);
    assert_eq!(summary["network"]["connected"], true);
    assert_eq!(
        summary["input_devices"]["devices"][0]["descriptor"]["id"],
        "switch"
    );
    assert_eq!(summary["upscaler"]["state"], "not_rendered");
    assert_eq!(summary["upscaler"]["requested"]["mode"], "dlss_sr");
    assert_eq!(summary["frame_generation"]["state"], "not_rendered");
    assert_eq!(summary["frame_generation"]["requested"]["multiplier"], 2);
    assert_eq!(summary["localization"]["locale"], "fr");
    assert_eq!(summary["localization"]["missing_messages"], 1);
    assert_eq!(
        summary["render_pipeline"]["features"][0]["name"],
        "engine.opaque"
    );
    assert_eq!(
        summary["render_pipeline"]["overrides"]["engine.opaque"],
        false
    );
    assert_eq!(summary["acoustics"]["state"], "Active");
    assert_eq!(summary["acoustics"]["active"], "Cpu");
    assert_eq!(
        summary["behavior_trees"]["agents"][0]["active_path"][1],
        "seq"
    );
    assert_eq!(
        summary["behavior_trees"]["agents"][0]["blackboard"]["alarm"],
        true
    );
    assert_eq!(
        summary["state_trees"]["agents"][0]["active_path"][1],
        "chase"
    );
    assert_eq!(
        summary["state_trees"]["agents"][0]["transitions"][0]["to"],
        "chase"
    );
    assert_eq!(summary["telemetry_omitted"], json!([]));
    let oversized_acoustics = summarize(
        &json!({"scene":{"acoustics":{"fallback_reason":"x".repeat(65_536)}}}),
        None,
        50,
    );
    assert!(oversized_acoustics["acoustics"].is_null());
    assert_eq!(
        oversized_acoustics["telemetry_omitted"],
        json!(["acoustics"])
    );
    let oversized = summarize(
        &json!({"scene":{"input_devices":{"label":"x".repeat(65_536)}}}),
        None,
        50,
    );
    assert!(oversized["input_devices"].is_null());
    assert_eq!(oversized["telemetry_omitted"], json!(["input_devices"]));
    let oversized_upscaler = summarize(
        &json!({"scene":{"upscaler":{"fallback_reason":"x".repeat(65_536)}}}),
        None,
        50,
    );
    assert!(oversized_upscaler["upscaler"].is_null());
    assert_eq!(oversized_upscaler["telemetry_omitted"], json!(["upscaler"]));
    let oversized_pipeline = summarize(
        &json!({"scene":{"render_pipeline":{"unknown_overrides":["x".repeat(65_536)]}}}),
        None,
        50,
    );
    assert!(oversized_pipeline["render_pipeline"].is_null());
    assert_eq!(
        oversized_pipeline["telemetry_omitted"],
        json!(["render_pipeline"])
    );
    let oversized_trees = summarize(
        &json!({"scene":{"behavior_trees":{"agents":["x".repeat(65_536)]}}}),
        None,
        50,
    );
    assert!(oversized_trees["behavior_trees"].is_null());
    assert_eq!(
        oversized_trees["telemetry_omitted"],
        json!(["behavior_trees"])
    );
    let oversized_state_trees = summarize(
        &json!({"scene":{"state_trees":{"agents":["x".repeat(65_536)]}}}),
        None,
        50,
    );
    assert!(oversized_state_trees["state_trees"].is_null());
    assert_eq!(
        oversized_state_trees["telemetry_omitted"],
        json!(["state_trees"])
    );
    let absent = summarize(&json!({}), None, 50);
    assert!(absent["behavior_trees"].is_null());
    assert!(absent["state_trees"].is_null());
    assert!(absent["upscaler"].is_null());
    assert!(absent["render_pipeline"].is_null());
    assert!(absent["frame_generation"].is_null());
    assert!(absent["acoustics"].is_null());
    let oversized_generation = summarize(
        &json!({"scene":{"frame_generation":{"fallback_reason":"x".repeat(65_536)}}}),
        None,
        50,
    );
    assert!(oversized_generation["frame_generation"].is_null());
    assert_eq!(
        oversized_generation["telemetry_omitted"],
        json!(["frame_generation"])
    );
    assert!(absent["profile"].is_null());
    assert!(absent["localization"].is_null());
    assert!(absent["input_devices"].is_null());
    assert_eq!(absent["telemetry_omitted"], json!([]));
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

fn run_envelope() -> Vec<u8> {
    format!(
        "{}\n",
        json!({"schema":1,"ok":true,"command":"run","result":{"ticks":1}})
    )
    .into_bytes()
}

#[test]
fn output_beyond_the_capture_cap_is_reported_not_silently_cut() {
    // Within the cap nothing is truncated.
    let (bytes, truncated) = read_capped(std::io::Cursor::new(vec![b'x'; 10]), 10).unwrap();
    assert_eq!((bytes.len(), truncated), (10, false));
    // One byte more is detected; the reader still drains the rest.
    let (bytes, truncated) = read_capped(std::io::Cursor::new(vec![b'x'; 11]), 10).unwrap();
    assert_eq!((bytes.len(), truncated), (10, true));
}

#[test]
fn a_truncated_run_output_is_output_too_large_not_run_no_result() {
    // A valid envelope exists, but output that overflowed the cap lost its tail.
    let ok = parse_run_output(&run_envelope(), false, Some(0)).unwrap();
    assert_eq!(ok["ticks"], 1);
    let failure = parse_run_output(&run_envelope(), true, Some(0)).unwrap_err();
    assert_eq!(failure.code, "output_too_large");
    assert!(
        failure.message.contains(&MAX_STDOUT_BYTES.to_string()),
        "{}",
        failure.message
    );
    // Without truncation a missing envelope is still run_no_result.
    let failure = parse_run_output(b"noise only\n", false, Some(1)).unwrap_err();
    assert_eq!(failure.code, "run_no_result");
}

#[test]
fn scheduled_run_checks_exec_gate_before_events_and_never_needs_authoring_write() {
    use engine_mcp::{Access, Limits, Tool, ToolContext};
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits::default();
    let tool = RunTool(Arc::new(State::default()));
    let args = json!({"ticks":1,"events":[{"tick":5,"name":"test"}]})
        .as_object()
        .unwrap()
        .clone();
    for write in [false, true] {
        let cx = ToolContext {
            root: dir.path(),
            limits: &limits,
            access: Access { write, exec: false },
        };
        assert_eq!(tool.call(&cx, &args).unwrap_err().code, "exec_disabled");
    }
    let cx = ToolContext {
        root: dir.path(),
        limits: &limits,
        access: Access {
            write: false,
            exec: true,
        },
    };
    let error = tool.call(&cx, &args).unwrap_err();
    assert_ne!(error.code, "write_disabled");
    assert!(error.message.contains("events"));
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "validation must precede build or writes"
    );
}
#[test]
fn scheduled_events_stage_only_ephemeral_input_and_round_trip_through_cli_parser() {
    use engine_mcp::schema::Args;
    let input =
        json!({"events":[{"tick":0,"name":"game.hit","target":"player","payload":{"damage":4}}]});
    let schedule = parse_events(&Args(input.as_object().unwrap()), 2).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut args = vec!["project.json".into(), "--ticks".into(), "2".into()];
    stage_events(&schedule, directory.path(), &mut args).unwrap();
    let parsed = crate::arguments::parse_run(&args).unwrap();
    assert_eq!(parsed.events.entries(), schedule.entries());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
#[test]
fn event_trace_summary_reports_truncation_and_total_count() {
    let events: Vec<_> = (0..120)
        .map(|tick| json!({"tick":tick,"name":"test","payload":{}}))
        .collect();
    let summary = summarize(
        &json!({"event_trace":{"events":events,"count":150,"truncated":true}}),
        None,
        50,
    );
    assert_eq!(summary["event_trace"].as_array().unwrap().len(), 100);
    assert_eq!(summary["event_count"], 150);
    assert_eq!(summary["event_trace_truncated"], true);
    let summary = summarize(
        &json!({"event_trace":{"events":[{"tick":0,"name":"large","payload":"x".repeat(65536)}],"count":1,"truncated":false}}),
        None,
        50,
    );
    assert!(summary["event_trace"].as_array().unwrap().is_empty());
    assert_eq!(summary["event_trace_truncated"], true);
    assert_eq!(summary["event_count"], 1);
}
