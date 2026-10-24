//! Input parsing and child-process execution shared by the run and screenshot tools.

use super::support::*;
use super::*;
use crate::compiler::{BuildArtifact, BuildFailure, RunningGeneration};
use engine_mcp::{
    ToolContext, ToolFailure,
    schema::{self, Args},
};
use engine_project::Project;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    thread,
    time::Duration,
};

pub(super) fn inputs_schema() -> Value {
    schema::array_of(
        "Scripted input: each entry holds `action` (see input_actions) for `duration` ticks \
         starting at `tick`. At most 400 ticks of input in total.",
        json!({"type": "object", "properties": {
            "action": schema::string("Input action name."),
            "tick": schema::integer("First tick (0-based) the action is held.", 0, MAX_TICKS as i64 - 1, None),
            "duration": schema::integer("Ticks to hold the action.", 1, 400, Some(1))},
            "required": ["action", "tick"], "additionalProperties": false}),
        400,
    )
}

pub(super) fn parse_inputs(
    project: &Project,
    args: &Args<'_>,
    ticks: u64,
) -> Result<Vec<(String, u64)>, ToolFailure> {
    let mut expanded = Vec::new();
    let Some(raw) = args.raw("inputs").filter(|v| !v.is_null()) else {
        return Ok(expanded);
    };
    let Some(items) = raw.as_array() else {
        return Err(schema::invalid(
            "inputs",
            "an array of {action, tick, duration}",
        ));
    };
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| schema::invalid("inputs", "an array of {action, tick, duration}"))?;
        let entry = Args(object);
        if let Some(extra) = object
            .keys()
            .find(|k| !matches!(k.as_str(), "action" | "tick" | "duration"))
        {
            return Err(schema::invalid(
                "inputs",
                &format!("objects with action, tick, duration (found `{extra}`)"),
            ));
        }
        let action = entry.string("action")?;
        let tick = entry.integer("tick", 0, ticks.saturating_sub(1), u64::MAX)?;
        let duration = entry.integer("duration", 1, 400, 1)?;
        if !project.input_bindings.contains_key(action)
            && !project.input_actions.contains_key(action)
        {
            let declared: Vec<&str> = project.input_bindings.keys().map(String::as_str).collect();
            return Err(ToolFailure::new(
                "unknown_action",
                format!("input action `{action}` is not declared by the project"),
            )
            .with_hint("Use one of the declared actions (see input_actions).")
            .with_details(json!({"declared": declared})));
        }
        for offset in 0..duration {
            if tick + offset < ticks {
                expanded.push((action.to_owned(), tick + offset));
            }
        }
        if expanded.len() > MAX_INPUTS {
            return Err(schema::invalid(
                "inputs",
                &format!("at most {MAX_INPUTS} ticks of input in total"),
            ));
        }
    }
    Ok(expanded)
}

pub(super) fn base_arguments(
    cx: &ToolContext<'_>,
    ticks: u64,
    inputs: &[(String, u64)],
    user_data: &Path,
    scene: Option<&PathBuf>,
) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec![
        cx.root.join("project.json").into(),
        "--ticks".into(),
        ticks.to_string().into(),
        "--user-data".into(),
        user_data.into(),
    ];
    for (action, tick) in inputs {
        arguments.push("--input".into());
        arguments.push(format!("{action}:{tick}").into());
    }
    if let Some(scene) = scene {
        arguments.push("--scene-override".into());
        arguments.push(scene.into());
    }
    arguments
}

/// Runs a generation to completion (or the deadline) and returns the parsed
/// `run` envelope's `result` object.
pub(super) fn execute(
    artifact: BuildArtifact,
    arguments: &[OsString],
    timeout: Duration,
) -> Result<Value, ToolFailure> {
    let spawn_error = |error: BuildFailure| {
        ToolFailure::new(
            error.code.clone(),
            format!("the generation could not be started: {}", error.message),
        )
        .with_hint("Rebuild with project_build and retry.")
    };
    let mut child =
        RunningGeneration::prepare_with_output(artifact, arguments, &AtomicBool::new(false), true)
            .map_err(spawn_error)?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| ToolFailure::new("tooling_error", "generation stdout was not captured"))?;
    let reader = thread::spawn(move || read_capped(stdout, MAX_STDOUT_BYTES));
    child.start().map_err(spawn_error)?;
    let (exit, timed_out) = with_deadline(timeout, |cancel| child.wait(cancel));
    // Dropping the generation ends any descendant still holding the pipe.
    drop(child);
    let (output, truncated) = reader.join().ok().and_then(Result::ok).unwrap_or_default();
    if timed_out {
        return Err(ToolFailure::new(
            "run_timeout",
            format!(
                "the run exceeded {} seconds and was stopped",
                timeout.as_secs()
            ),
        )
        .with_hint(
            "Lower ticks, or look for scripts that never finish a tick; raise timeout_seconds \
             if the simulation is legitimately slow.",
        ));
    }
    let exit_code = exit.ok().and_then(|e| e.code);
    parse_run_output(&output, truncated, exit_code)
}

/// Reads at most `cap` bytes from `source` and keeps draining (discarding) the
/// rest, so a chatty child never blocks on a full pipe. The flag reports whether
/// anything beyond `cap` was produced.
pub(super) fn read_capped(mut source: impl Read, cap: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    (&mut source).take(cap).read_to_end(&mut bytes)?;
    let truncated = std::io::copy(&mut source, &mut std::io::sink())? > 0;
    Ok((bytes, truncated))
}

/// Finds the final `run` envelope in a generation's captured stdout. Output
/// that exceeded the capture cap lost its tail, where the envelope lives, so it
/// is reported as such instead of as a missing result.
pub(super) fn parse_run_output(
    output: &[u8],
    truncated: bool,
    exit_code: Option<i32>,
) -> Result<Value, ToolFailure> {
    if truncated {
        return Err(ToolFailure::new(
            "output_too_large",
            format!(
                "the generation wrote more than {MAX_STDOUT_BYTES} bytes to stdout, so the run \
                 result was lost"
            ),
        )
        .with_hint(
            "Reduce what the game prints (println!, dbg!) and run again; use the engine log \
             instead of stdout for diagnostics.",
        )
        .with_details(json!({"limit_bytes": MAX_STDOUT_BYTES, "exit_code": exit_code})));
    }
    let text = String::from_utf8_lossy(output);
    let envelope = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|v| v["schema"] == 1 && v["command"] == "run");
    let Some(envelope) = envelope else {
        return Err(ToolFailure::new(
            "run_no_result",
            format!("the generation exited ({exit_code:?}) without reporting a run result"),
        )
        .with_hint("The game may have crashed; check the server's stderr output."));
    };
    if envelope["ok"] == true {
        return Ok(envelope["result"].clone());
    }
    Err(run_failure(&envelope, exit_code))
}

/// Maps a failed `run` envelope to a structured failure. A missing GPU adapter
/// becomes the dedicated code `gpu_unavailable`.
pub(super) fn run_failure(envelope: &Value, exit_code: Option<i32>) -> ToolFailure {
    let list: Vec<Value> = envelope["result"]["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let first = list.first().cloned().unwrap_or(Value::Null);
    let code = first["code"].as_str().unwrap_or("runtime.failed");
    let (code, hint) = if code == "render.gpu.unavailable" {
        (
            "gpu_unavailable",
            "No usable GPU adapter exists on this host (typical for CI or a VM). Use \
             project_run_headless to inspect simulation state instead.",
        )
    } else {
        (
            code,
            "Fix the reported problem (see diagnostics) and retry; project_validate checks the \
             authored data without running the game.",
        )
    };
    ToolFailure::new(
        code.to_owned(),
        first["message"]
            .as_str()
            .unwrap_or("the run failed")
            .to_owned(),
    )
    .with_hint(hint)
    .with_diagnostics(list)
    .with_details(json!({"exit_code": exit_code}))
}

pub(super) fn scene_override(
    cx: &ToolContext<'_>,
    args: &Args<'_>,
) -> Result<Option<PathBuf>, ToolFailure> {
    match args.raw("scene").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(value) => cx.existing_file("scene", value).map(Some),
    }
}

pub(super) fn timeouts(args: &Args<'_>) -> Result<(Duration, Duration), ToolFailure> {
    Ok((
        Duration::from_secs(args.integer(
            "build_timeout_seconds",
            10,
            MAX_BUILD_TIMEOUT,
            DEFAULT_BUILD_TIMEOUT,
        )?),
        Duration::from_secs(args.integer(
            "timeout_seconds",
            5,
            MAX_RUN_TIMEOUT,
            DEFAULT_RUN_TIMEOUT,
        )?),
    ))
}

pub(super) fn timeout_properties() -> Vec<(&'static str, Value)> {
    vec![
        (
            "rebuild",
            schema::boolean(
                "Compile before running. Set false to reuse the generation built earlier in this \
                 server session (faster when only scene data changed).",
                true,
            ),
        ),
        (
            "build_timeout_seconds",
            schema::integer(
                "Build time limit.",
                10,
                MAX_BUILD_TIMEOUT as i64,
                Some(DEFAULT_BUILD_TIMEOUT as i64),
            ),
        ),
        (
            "timeout_seconds",
            schema::integer(
                "Run time limit; the game is stopped when exceeded.",
                5,
                MAX_RUN_TIMEOUT as i64,
                Some(DEFAULT_RUN_TIMEOUT as i64),
            ),
        ),
    ]
}

/// Optional bounded event input. Parsing happens before compilation or launch.
pub(super) fn parse_events(
    args: &Args<'_>,
    ticks: u64,
) -> Result<engine_project::EventSchedule, ToolFailure> {
    let Some(value) = args.raw("events") else {
        return Ok(Default::default());
    };
    let schedule = engine_project::EventSchedule::from_json(
        &serde_json::to_vec(value).map_err(|e| schema::invalid("events", &e.to_string()))?,
    )
    .map_err(|e| schema::invalid("events", &e.to_string()))?;
    schedule
        .validate_run(ticks)
        .map_err(|e| schema::invalid("events", &e.to_string()))?;
    Ok(schedule)
}
pub(super) fn events_schema() -> Value {
    schema::array_of(
        "Ordered scheduled events, delivered before each zero-based tick. Stable source/target IDs resolve in the active scene. At most 1 MiB total, 1024 per tick, 64 KiB payload each.",
        json!({"type":"object","properties":{
            "tick":schema::integer("Delivery tick, zero-based.",0,MAX_TICKS as i64-1,None),
            "name":schema::string("Registered event name, up to 128 bytes."),
            "source":schema::string("Optional stable source entity ID."),
            "target":schema::string("Optional stable target entity ID; absent broadcasts."),
            "payload":{}
        },"required":["tick","name"],"additionalProperties":false}),
        engine_project::scheduled_events::MAX_SCHEDULE_EVENTS,
    )
}
/// Ephemeral execution input lives with the run's disposable user data and does
/// not modify authored project files or require authoring-write access.
pub(super) fn stage_events(
    schedule: &engine_project::EventSchedule,
    directory: &Path,
    arguments: &mut Vec<OsString>,
) -> Result<(), ToolFailure> {
    if !schedule.is_empty() {
        let path = directory.join("scheduled-events.json");
        let bytes = serde_json::to_vec(schedule)
            .map_err(|e| ToolFailure::new("tooling_error", e.to_string()))?;
        std::fs::write(&path, bytes)
            .map_err(|e| ToolFailure::new("tooling_error", e.to_string()))?;
        arguments.extend(["--events".into(), path.into_os_string()]);
    }
    Ok(())
}
