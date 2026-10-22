//! Shared helpers: deadlines, text clipping, error mapping and build diagnostics.

use super::*;
use crate::{
    cli::{compiler_diagnostic, rust_crate},
    compiler::BuildFailure,
};
use engine_core::anyhow;
use engine_mcp::ToolFailure;
use engine_project::Project;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Runs `work` with a watchdog that raises the cancel flag after `timeout`.
/// Returns the result and whether the deadline fired.
pub(super) fn with_deadline<T>(
    timeout: Duration,
    work: impl FnOnce(&AtomicBool) -> T,
) -> (T, bool) {
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

pub(super) fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    format!("{}…", &text[..text.floor_char_boundary(limit)])
}

pub(super) fn tail(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    &text[text.ceil_char_boundary(text.len() - limit)..]
}

pub(super) fn from_anyhow(error: anyhow::Error) -> ToolFailure {
    match error.downcast::<engine_project::Error>() {
        Ok(project) => ToolFailure::from(project),
        Err(other) => ToolFailure::new("tooling_error", format!("{other:#}")),
    }
}

pub(super) fn require_crate(project: &Project) -> Result<(), ToolFailure> {
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
pub(super) fn newest_generation(cache: &Path) -> Option<String> {
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
pub(super) fn diagnostics(raw: &[Value]) -> (Vec<Value>, usize, usize) {
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

pub(super) fn build_failure(
    error: &BuildFailure,
    timed_out: bool,
    last_good: Option<String>,
) -> ToolFailure {
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
