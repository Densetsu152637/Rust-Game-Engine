//! The `project_build` tool and generation reuse.

use super::support::*;
use super::*;
use crate::{
    cli::{compile_request, prune},
    compiler::BuildArtifact,
};
use engine_mcp::{
    AccessLevel, Annotations, Tool, ToolContext, ToolDefinition, ToolFailure, ToolOutcome,
    ToolResult,
    schema::{self, Args},
};
use engine_project::Project;
use serde_json::{Map, Value, json};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) struct Built {
    pub(super) artifact: BuildArtifact,
    pub(super) reused: bool,
    pub(super) millis: u128,
    pub(super) warnings: Vec<Value>,
    pub(super) warning_count: usize,
}

/// Compiles (or, when allowed, reuses) a generation for `project`.
pub(super) fn obtain_generation(
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
        tracing::info!(generation = %artifact.generation, "reusing the last built generation");
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

pub(super) fn build_json(cx: &ToolContext<'_>, built: &Built) -> Value {
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

pub(super) struct BuildTool(pub(super) Arc<State>);

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
