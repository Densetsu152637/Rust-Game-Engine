//! The `project_run_headless` tool.

use super::*;
use super::{build::*, exec::*, support::*};
use engine_mcp::{
    AccessLevel, Annotations, Tool, ToolContext, ToolDefinition, ToolFailure, ToolOutcome,
    ToolResult,
    schema::{self, Args},
};
use serde_json::{Map, Value, json};
use std::{sync::Arc, time::Instant};

// ---- project_run_headless ---------------------------------------------------------------

pub(super) struct RunTool(pub(super) Arc<State>);

impl Tool for RunTool {
    fn definition(&self) -> ToolDefinition {
        let mut properties = vec![
            (
                "ticks",
                schema::integer(
                    "Fixed 1/60 s simulation ticks to run.",
                    1,
                    MAX_TICKS as i64,
                    Some(60),
                ),
            ),
            ("inputs", inputs_schema()),
            ("events", events_schema()),
            (
                "scene",
                schema::string("Project-relative scene to run instead of the startup scene."),
            ),
            (
                "entities",
                schema::array_of(
                    "Report only these entity IDs (default: all, capped by max_entities).",
                    json!({"type": "string"}),
                    200,
                ),
            ),
            (
                "max_entities",
                schema::integer("Most entities to report.", 1, 200, Some(50)),
            ),
        ];
        properties.extend(timeout_properties());
        ToolDefinition {
            name: "project_run_headless".into(),
            title: "Run headless".into(),
            description: "Build the project (unless rebuild is false) and run it headlessly for N \
                fixed ticks with optional scripted input and stable-ID scheduled events, without a window, GPU or audio device. \
                Returns a bounded summary: the final authored state of the entities (component \
                values after scripts ran), script logs, trigger events and counters. Runs use a \
                throw-away user-data directory, so saves are not touched. Executes the project's \
                native game code with your privileges."
                .into(),
            input_schema: schema::object(properties, &[]),
            output_schema: Some(schema::output(vec![
                (
                    "ticks",
                    json!({"type": "integer", "description": "Ticks actually simulated."}),
                ),
                ("fixed_delta", json!({"type": "number"})),
                (
                    "scene_id",
                    schema::string("Active scene at the end of the run."),
                ),
                ("live_entities", json!({"type": "integer"})),
                ("active_scripts", json!({"type": "integer"})),
                ("entity_count", json!({"type": "integer"})),
                (
                    "entities",
                    json!({"type": "array", "items": {"type": "object"}}),
                ),
                ("entities_truncated", json!({"type": "boolean"})),
                (
                    "logs",
                    json!({"type": "array", "items": {"type": "string"}}),
                ),
                ("log_count", json!({"type": "integer"})),
                ("logs_truncated", json!({"type": "boolean"})),
                (
                    "triggers",
                    json!({"type": "array", "items": {"type": "object"}}),
                ),
                ("event_trace", json!({"type":"array","items":{"type":"object"}})),
                ("event_count", json!({"type":"integer"})),
                ("event_trace_truncated", json!({"type":"boolean"})),
                ("profile", json!({"type":["object","null"]})),
                ("profile_frames_truncated", json!({"type":"boolean"})),
                ("network", json!({"type":["object","null"]})),
                ("input_devices", json!({"type":["object","null"]})),
                ("upscaler", json!({"type":["object","null"], "description":
                    "Temporal upscaler request and what the renderer did (state not_rendered in headless runs)."})),
                ("frame_generation", json!({"type":["object","null"], "description":
                    "Frame generation request and what the renderer did (state not_rendered in headless runs)."})),
                ("acoustics", json!({"type":["object","null"], "description":
                    "Acoustic propagation status: requested and active backend, state and fallback reason, world statistics, update counters and timings, mixer counters and up to 32 per-source entries. Null without an engine.AcousticEnvironment."})),
                ("telemetry_omitted", json!({"type":"array","items":{"type":"string"}})),
                ("build", json!({"type": "object"})),
                ("run_millis", json!({"type": "integer"})),
            ])),
            annotations: Annotations {
                read_only: false,
                destructive: true,
                idempotent: false,
                open_world: true,
            },
            access: AccessLevel::Exec,
        }
    }

    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
        cx.require(AccessLevel::Exec)?;
        let a = Args(args);
        let ticks = a.integer("ticks", 1, MAX_TICKS, 60)?;
        let events = parse_events(&a, ticks)?;
        let wanted = a.string_array("entities")?;
        let max_entities = a.integer("max_entities", 1, 200, 50)? as usize;
        let rebuild = a.boolean("rebuild", true)?;
        let (build_timeout, run_timeout) = timeouts(&a)?;
        let scene = scene_override(cx, &a)?;
        let project = cx.project()?;
        let inputs = parse_inputs(&project, &a, ticks)?;
        let built = obtain_generation(&self.0, &project, rebuild, false, build_timeout)?;
        let user_data =
            tempfile::tempdir().map_err(|e| ToolFailure::new("tooling_error", e.to_string()))?;
        let mut arguments = base_arguments(cx, ticks, &inputs, user_data.path(), scene.as_ref());
        stage_events(&events, user_data.path(), &mut arguments)?;
        arguments.push("--headless".into());
        let build = build_json(cx, &built);
        let started = Instant::now();
        let report = execute(built.artifact, &arguments, run_timeout)?;
        let mut summary = summarize(&report, wanted.as_deref(), max_entities);
        summary["build"] = build;
        summary["run_millis"] = json!(started.elapsed().as_millis());
        Ok(ToolResult::new(summary))
    }
}

/// A bounded view of a run report.
pub(super) fn summarize(report: &Value, wanted: Option<&[&str]>, max_entities: usize) -> Value {
    let snapshot = &report["scene"];
    let all = snapshot["scene"]["entities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let selected: Vec<&Value> = all
        .iter()
        .filter(|e| wanted.is_none_or(|ids| e["id"].as_str().is_some_and(|id| ids.contains(&id))))
        .collect();
    let mut entities = Vec::new();
    let mut bytes = 0;
    let mut truncated = false;
    for entity in selected.iter().take(max_entities) {
        bytes += entity.to_string().len();
        if bytes > MAX_ENTITY_BYTES {
            truncated = true;
            break;
        }
        entities.push((*entity).clone());
    }
    truncated |= entities.len() < selected.len();
    let logs: Vec<String> = snapshot["logs"]
        .as_array()
        .map(|logs| {
            logs.iter()
                .filter_map(Value::as_str)
                .map(|l| clip(l, 500))
                .collect()
        })
        .unwrap_or_default();
    let log_count = logs.len();
    let shown: Vec<&String> = logs
        .iter()
        .skip(log_count.saturating_sub(MAX_LOGS))
        .collect();
    let triggers: Vec<&Value> = snapshot["triggers"]
        .as_array()
        .map(|t| t.iter().take(100).collect())
        .unwrap_or_default();
    let mut event_trace = Vec::new();
    let mut trace_bytes = 0;
    let source_events = report["event_trace"]["events"].as_array();
    if let Some(events) = source_events {
        for event in events.iter().take(100) {
            let size = event.to_string().len();
            if trace_bytes + size > 64 * 1024 {
                break;
            }
            trace_bytes += size;
            event_trace.push(event);
        }
    }
    let trace_truncated = report["event_trace"]["truncated"]
        .as_bool()
        .unwrap_or(false)
        || source_events.is_some_and(|events| events.len() > event_trace.len());
    let mut profile = report["profile"].clone();
    let mut profile_frames_truncated = false;
    if let Some(frames) = profile.get_mut("frames").and_then(Value::as_array_mut) {
        profile_frames_truncated = frames.len() > 32;
        frames.drain(..frames.len().saturating_sub(32));
    }
    let mut telemetry_omitted = Vec::new();
    let mut telemetry = |name: &str, value: &Value| {
        if value.to_string().len() > 64 * 1024 {
            telemetry_omitted.push(name.to_owned());
            Value::Null
        } else {
            value.clone()
        }
    };
    let profile = telemetry("profile", &profile);
    let network = telemetry("network", &snapshot["network"]);
    let input_devices = telemetry("input_devices", &snapshot["input_devices"]);
    let upscaler = telemetry("upscaler", &snapshot["upscaler"]);
    let frame_generation = telemetry("frame_generation", &snapshot["frame_generation"]);
    let acoustics = telemetry("acoustics", &snapshot["acoustics"]);
    json!({
        "upscaler": upscaler, "frame_generation": frame_generation, "acoustics": acoustics,
        "profile": profile, "profile_frames_truncated": profile_frames_truncated,
        "network": network, "input_devices": input_devices, "telemetry_omitted": telemetry_omitted,
        "event_trace":event_trace, "event_count":report["event_trace"]["count"].as_u64().unwrap_or(0),
        "event_trace_truncated":trace_truncated,
        "ticks": report["ticks"], "fixed_delta": report["fixed_delta"],
        "scene_id": snapshot["scene"]["scene_id"],
        "scene_revision": snapshot["scene_revision"],
        "live_entities": snapshot["live_entities"], "active_scripts": snapshot["active_scripts"],
        "entity_count": all.len(), "entities": entities, "entities_truncated": truncated,
        "logs": shown, "log_count": log_count, "logs_truncated": log_count > MAX_LOGS,
        "triggers": triggers,
    })
}

// ---- render_screenshot ------------------------------------------------------------------
