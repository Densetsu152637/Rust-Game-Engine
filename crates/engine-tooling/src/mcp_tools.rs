//! Execution tools for the MCP server (`engine mcp <root> --allow-exec`).
//!
//! `engine-mcp` cannot depend on the native compiler, so these tools live here
//! and are registered through [`engine_mcp::ToolProvider`]. They reuse the real
//! pipeline: [`NativeCompiler`] builds a unique generation (a failed build never
//! touches earlier generations), and runs execute the generation as a child
//! process through [`RunningGeneration`], exactly like `engine run`.
//!
//! Project code runs with the host user's privileges. Every call is bounded by a
//! timeout, output is size-capped, runs use a throw-away user-data directory and
//! only confined project files are passed to the child.

use crate::{
    arguments::{DEFAULT_SCREENSHOT_SIZE, MAX_SCREENSHOT_EDGE},
    cli::{compile_request, compiler_diagnostic, prune, rust_crate},
    compiler::{BuildArtifact, BuildFailure, RunningGeneration},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use engine_core::anyhow;
use engine_mcp::{
    AccessLevel, Annotations, Content, Tool, ToolContext, ToolDefinition, ToolFailure, ToolOutcome,
    ToolProvider, ToolResult,
    schema::{self, Args},
};
use engine_project::Project;
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

const DEFAULT_BUILD_TIMEOUT: u64 = 600;
const MAX_BUILD_TIMEOUT: u64 = 1800;
const DEFAULT_RUN_TIMEOUT: u64 = 120;
const MAX_RUN_TIMEOUT: u64 = 600;
const MAX_TICKS: u64 = 100_000;
const MAX_SCREENSHOT_TICKS: u64 = 10_000;
const MAX_INPUTS: usize = 400;
const MAX_STDOUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 30;
const MAX_ENTITY_BYTES: usize = 300_000;
const MAX_LOGS: usize = 200;
const CLIP: usize = 2000;
/// Head-room for the JSON envelope around an image content block.
const IMAGE_OVERHEAD: usize = 16 * 1024;

#[derive(Default)]
struct State {
    /// The newest generation built by this server, reusable by later runs.
    last_build: Mutex<Option<BuildArtifact>>,
}

/// Provides `project_build`, `project_run_headless` and `render_screenshot`.
/// Register it only for servers started with exec access.
#[derive(Clone, Default)]
pub struct ExecTools {
    state: Arc<State>,
}

impl ExecTools {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolProvider for ExecTools {
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(BuildTool(self.state.clone())),
            Box::new(RunTool(self.state.clone())),
            Box::new(ScreenshotTool(self.state.clone())),
        ]
    }
}

/// Runs `work` with a watchdog that raises the cancel flag after `timeout`.
/// Returns the result and whether the deadline fired.
fn with_deadline<T>(timeout: Duration, work: impl FnOnce(&AtomicBool) -> T) -> (T, bool) {
    let cancel = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let start = Instant::now();
            while !finished.load(Ordering::Acquire) {
                if start.elapsed() >= timeout {
                    cancel.store(true, Ordering::Release);
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let value = work(&cancel);
        finished.store(true, Ordering::Release);
        (value, cancel.load(Ordering::Acquire))
    })
}

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    format!("{}…", &text[..text.floor_char_boundary(limit)])
}

fn tail(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    &text[text.ceil_char_boundary(text.len() - limit)..]
}

fn from_anyhow(error: anyhow::Error) -> ToolFailure {
    match error.downcast::<engine_project::Error>() {
        Ok(project) => ToolFailure::from(project),
        Err(other) => ToolFailure::new("tooling_error", format!("{other:#}")),
    }
}

fn require_crate(project: &Project) -> Result<(), ToolFailure> {
    match rust_crate(project) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(ToolFailure::new(
            "rust_crate_missing",
            "the project has no Rust gameplay crate to build or run",
        )
        .with_hint(
            "Add `\"rust\": {\"manifest\": \"Cargo.toml\"}` to project.json and create the \
             crate with a `pub fn configure(app: &mut EngineApp)` entry point.",
        )),
        Err(error) => Err(from_anyhow(error)),
    }
}

/// Newest complete generation in the cache, by directory name.
fn newest_generation(cache: &Path) -> Option<String> {
    let mut found: Vec<(SystemTime, String)> = std::fs::read_dir(cache)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().join("generation.json").is_file())
        .filter_map(|entry| {
            Some((
                entry.metadata().ok()?.modified().ok()?,
                entry.file_name().to_string_lossy().into_owned(),
            ))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, name)| name)
}

/// Compiler diagnostics in engine shape: errors first, then warnings, bounded.
fn diagnostics(raw: &[Value]) -> (Vec<Value>, usize, usize) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for message in raw {
        let level = message["level"].as_str().unwrap_or("");
        if !matches!(level, "error" | "warning") {
            continue;
        }
        let mut item = compiler_diagnostic(message);
        if let Some(rendered) = item["rendered"].as_str() {
            item["rendered"] = json!(clip(rendered, CLIP));
        }
        if level == "error" {
            errors.push(item);
        } else {
            warnings.push(item);
        }
    }
    let (error_count, warning_count) = (errors.len(), warnings.len());
    errors.extend(warnings);
    errors.truncate(MAX_DIAGNOSTICS);
    (errors, error_count, warning_count)
}

fn build_failure(error: &BuildFailure, timed_out: bool, last_good: Option<String>) -> ToolFailure {
    let (list, error_count, _) = diagnostics(&error.diagnostics);
    let (code, message) = if timed_out || error.cancelled {
        (
            "build_timeout".to_owned(),
            "the build exceeded its time limit and was cancelled".to_owned(),
        )
    } else if error_count > 0 {
        (
            error.code.clone(),
            format!("the native build failed with {error_count} error(s)"),
        )
    } else {
        (error.code.clone(), error.message.clone())
    };
    let hint = if timed_out || error.cancelled {
        "Raise timeout_seconds (the first build compiles the whole engine) or call project_build \
         again; cargo reuses its cache."
    } else {
        "Fix the compiler errors in diagnostics (file, path = line:column, rendered), then call \
         project_build again. Earlier generations are untouched."
    };
    let stderr = error.stderr.trim();
    ToolFailure::new(code, message)
        .with_hint(hint)
        .with_diagnostics(list)
        .with_details(json!({
            "exit_code": error.exit_code,
            "error_count": error_count,
            "last_good_generation": last_good,
            "stderr_tail": tail(stderr, 4000),
        }))
}

struct Built {
    artifact: BuildArtifact,
    reused: bool,
    millis: u128,
    warnings: Vec<Value>,
    warning_count: usize,
}

/// Compiles (or, when allowed, reuses) a generation for `project`.
fn obtain_generation(
    state: &State,
    project: &Project,
    rebuild: bool,
    release: bool,
    timeout: Duration,
) -> Result<Built, ToolFailure> {
    require_crate(project)?;
    if !rebuild
        && let Some(artifact) = state.last_build.lock().ok().and_then(|g| g.clone())
        && artifact.executable.is_file()
    {
        return Ok(Built {
            artifact,
            reused: true,
            millis: 0,
            warnings: Vec::new(),
            warning_count: 0,
        });
    }
    let (mut compiler, mut request) = compile_request(project).map_err(from_anyhow)?;
    request.release = release;
    let started = Instant::now();
    let (result, timed_out) = with_deadline(timeout, |cancel| compiler.compile(&request, cancel));
    match result {
        Ok(artifact) => {
            prune(&request, &artifact.directory);
            let (warnings, _, warning_count) = diagnostics(&artifact.diagnostics);
            if let Ok(mut slot) = state.last_build.lock() {
                *slot = Some(artifact.clone());
            }
            Ok(Built {
                artifact,
                reused: false,
                millis: started.elapsed().as_millis(),
                warnings,
                warning_count,
            })
        }
        Err(error) => Err(build_failure(
            &error,
            timed_out,
            newest_generation(&request.cache_root),
        )),
    }
}

fn build_json(cx: &ToolContext<'_>, built: &Built) -> Value {
    let relative = |path: &Path| {
        path.strip_prefix(cx.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    };
    json!({"generation": built.artifact.generation, "fingerprint": built.artifact.fingerprint,
        "executable": relative(&built.artifact.executable), "reused": built.reused,
        "build_millis": built.millis})
}

// ---- project_build ----------------------------------------------------------------------

struct BuildTool(Arc<State>);

impl Tool for BuildTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "project_build".into(),
            title: "Build native game".into(),
            description: "Compile the project's Rust gameplay crate together with the engine into \
                a native executable generation (cargo build). Returns the generation ID and \
                structured compiler diagnostics (code, severity, file, path = line:column, \
                message, rendered). A failed build never removes or replaces the last good \
                generation. Slow the first time (compiles the engine), incremental afterwards. \
                Build scripts and proc macros of the project's dependencies run with your \
                privileges and cargo may download crates."
                .into(),
            input_schema: schema::object(
                vec![
                    (
                        "release",
                        schema::boolean("Build with optimizations.", false),
                    ),
                    (
                        "timeout_seconds",
                        schema::integer(
                            "Cancel the build after this many seconds.",
                            10,
                            MAX_BUILD_TIMEOUT as i64,
                            Some(DEFAULT_BUILD_TIMEOUT as i64),
                        ),
                    ),
                ],
                &[],
            ),
            output_schema: Some(schema::output(vec![
                (
                    "generation",
                    schema::string("Generation directory name under .derived/native."),
                ),
                (
                    "fingerprint",
                    schema::string("Content fingerprint of the generation inputs."),
                ),
                (
                    "executable",
                    schema::string("Project-relative path of the built executable."),
                ),
                (
                    "profile",
                    schema::string_enum("Build profile.", &["dev", "release"]),
                ),
                ("build_millis", json!({"type": "integer"})),
                ("warning_count", json!({"type": "integer"})),
                (
                    "warnings",
                    json!({"type": "array", "items": {"type": "object"}}),
                ),
            ])),
            annotations: Annotations {
                read_only: false,
                destructive: false,
                idempotent: true,
                open_world: true,
            },
            access: AccessLevel::Exec,
        }
    }

    fn call(&self, cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
        cx.require(AccessLevel::Exec)?;
        let a = Args(args);
        let release = a.boolean("release", false)?;
        let timeout = a.integer(
            "timeout_seconds",
            10,
            MAX_BUILD_TIMEOUT,
            DEFAULT_BUILD_TIMEOUT,
        )?;
        let project = cx.project()?;
        let built = obtain_generation(
            &self.0,
            &project,
            true,
            release,
            Duration::from_secs(timeout),
        )?;
        let mut result = build_json(cx, &built);
        result["profile"] = json!(if release { "release" } else { "dev" });
        result["warning_count"] = json!(built.warning_count);
        result["warnings"] = json!(built.warnings);
        Ok(ToolResult::new(result))
    }
}

// ---- running generations ----------------------------------------------------------------

fn inputs_schema() -> Value {
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

fn parse_inputs(
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

fn base_arguments(
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
fn execute(
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
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_STDOUT_BYTES)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    child.start().map_err(spawn_error)?;
    let (exit, timed_out) = with_deadline(timeout, |cancel| child.wait(cancel));
    // Dropping the generation ends any descendant still holding the pipe.
    drop(child);
    let output = reader.join().ok().and_then(Result::ok).unwrap_or_default();
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
    let text = String::from_utf8_lossy(&output);
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
fn run_failure(envelope: &Value, exit_code: Option<i32>) -> ToolFailure {
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

fn scene_override(cx: &ToolContext<'_>, args: &Args<'_>) -> Result<Option<PathBuf>, ToolFailure> {
    match args.raw("scene").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(value) => cx.existing_file("scene", value).map(Some),
    }
}

fn timeouts(args: &Args<'_>) -> Result<(Duration, Duration), ToolFailure> {
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

fn timeout_properties() -> Vec<(&'static str, Value)> {
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

// ---- project_run_headless ---------------------------------------------------------------

struct RunTool(Arc<State>);

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
                fixed ticks with optional scripted input, without a window, GPU or audio device. \
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
fn summarize(report: &Value, wanted: Option<&[&str]>, max_entities: usize) -> Value {
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
    json!({
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

struct ScreenshotTool(Arc<State>);

impl Tool for ScreenshotTool {
    fn definition(&self) -> ToolDefinition {
        let mut properties = vec![
            (
                "scene",
                schema::string("Project-relative scene to render instead of the startup scene."),
            ),
            (
                "ticks",
                schema::integer(
                    "Fixed 1/60 s ticks to simulate before capturing (1 shows the initial state).",
                    1,
                    MAX_SCREENSHOT_TICKS as i64,
                    Some(1),
                ),
            ),
            ("inputs", inputs_schema()),
            (
                "width",
                schema::integer(
                    "Image width in pixels.",
                    16,
                    2048,
                    Some(DEFAULT_SCREENSHOT_SIZE[0] as i64),
                ),
            ),
            (
                "height",
                schema::integer(
                    "Image height in pixels.",
                    16,
                    2048,
                    Some(DEFAULT_SCREENSHOT_SIZE[1] as i64),
                ),
            ),
        ];
        properties.extend(timeout_properties());
        ToolDefinition {
            name: "render_screenshot".into(),
            title: "Render screenshot".into(),
            description:
                "Build the project (unless rebuild is false), simulate N fixed ticks with \
                optional scripted input and render the final frame offscreen to a PNG, returned as \
                an MCP image plus metadata (size, GPU adapter, tick count). Uses the engine's real \
                renderer, so sprites, meshes and UI panels appear as in the game. Needs a GPU \
                adapter: on hosts without one it fails with the structured error `gpu_unavailable` \
                (use project_run_headless instead). Executes the project's native game code with \
                your privileges."
                    .into(),
            input_schema: schema::object(properties, &[]),
            output_schema: Some(schema::output(vec![
                ("width", json!({"type": "integer"})),
                ("height", json!({"type": "integer"})),
                ("ticks", json!({"type": "integer"})),
                ("png_bytes", json!({"type": "integer"})),
                ("scene_id", schema::string("Scene that was rendered.")),
                (
                    "adapter",
                    json!({"type": "object", "properties": {
                    "name": {"type": "string"}, "backend": {"type": "string"},
                    "device_type": {"type": "string"}}}),
                ),
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
        let ticks = a.integer("ticks", 1, MAX_SCREENSHOT_TICKS, 1)?;
        let width = a.integer("width", 16, 2048, DEFAULT_SCREENSHOT_SIZE[0] as u64)? as u32;
        let height = a.integer("height", 16, 2048, DEFAULT_SCREENSHOT_SIZE[1] as u64)? as u32;
        debug_assert!(width.max(height) <= MAX_SCREENSHOT_EDGE);
        let rebuild = a.boolean("rebuild", true)?;
        let (build_timeout, run_timeout) = timeouts(&a)?;
        let scene = scene_override(cx, &a)?;
        let project = cx.project()?;
        let inputs = parse_inputs(&project, &a, ticks)?;
        let built = obtain_generation(&self.0, &project, rebuild, false, build_timeout)?;
        let workspace =
            tempfile::tempdir().map_err(|e| ToolFailure::new("tooling_error", e.to_string()))?;
        let png_path = workspace.path().join("screenshot.png");
        let mut arguments = base_arguments(
            cx,
            ticks,
            &inputs,
            &workspace.path().join("user-data"),
            scene.as_ref(),
        );
        arguments.extend([
            "--screenshot".into(),
            png_path.clone().into(),
            "--screenshot-size".into(),
            format!("{width}x{height}").into(),
        ]);
        let build = build_json(cx, &built);
        let started = Instant::now();
        let report = execute(built.artifact, &arguments, run_timeout)?;
        let png = std::fs::read(&png_path).map_err(|_| {
            ToolFailure::new("run_no_result", "the run succeeded but produced no image")
        })?;
        let encoded = STANDARD.encode(&png);
        let budget = cx
            .limits
            .max_output_message_bytes
            .saturating_sub(IMAGE_OVERHEAD);
        if encoded.len() > budget {
            return Err(ToolFailure::new(
                "image_too_large",
                format!(
                    "the {width}x{height} image is {} KiB, above the {} KiB response budget",
                    encoded.len() / 1024,
                    budget / 1024
                ),
            )
            .with_hint("Request a smaller width and height (for example 320x180)."));
        }
        let shot = &report["screenshot"];
        let result = json!({
            "width": width, "height": height, "ticks": report["ticks"],
            "png_bytes": png.len(), "adapter": shot["adapter"],
            "scene_id": report["scene"]["scene"]["scene_id"],
            "build": build, "run_millis": started.elapsed().as_millis(),
        });
        Ok(ToolResult::new(result).with_content(Content::Image {
            data: encoded,
            mime_type: "image/png".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            raw.push(
                json!({"level":"warning","message":format!("w{i}"),"spans":[],"rendered":"w"}),
            );
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
}
