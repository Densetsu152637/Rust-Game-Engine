//! Usage errors and the mapping from errors to exit codes and diagnostics.

use crate::compiler::BuildFailure;
use engine_core::anyhow::Error;
use serde_json::{Value, json};

/// Invalid command-line usage (exit code 2).
#[derive(Debug)]
pub struct UsageError(pub String);
impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UsageError {}

pub(super) fn usage(message: impl Into<String>) -> Error {
    UsageError(message.into()).into()
}

pub(super) fn diagnostic(code: &str, message: impl Into<String>) -> Value {
    json!({"code": code, "severity": "error", "file": "", "path": "", "message": message.into()})
}

/// Maps an error to (exit code, diagnostics).
pub(super) fn classify(error: &Error) -> (i32, Vec<Value>) {
    if let Some(usage) = error.downcast_ref::<UsageError>() {
        return (2, vec![diagnostic("command.arguments", usage.0.clone())]);
    }
    if let Some(project) = error.downcast_ref::<engine_project::Error>() {
        let code = if project
            .diagnostics
            .iter()
            .any(|d| d.code.starts_with("command."))
        {
            2
        } else {
            1
        };
        let diagnostics = project
            .diagnostics
            .iter()
            .map(|d| serde_json::to_value(d).unwrap_or_else(|_| diagnostic(&d.code, &d.message)))
            .collect();
        return (code, diagnostics);
    }
    if let Some(build) = error.downcast_ref::<BuildFailure>() {
        let mut diagnostics = build
            .diagnostics
            .iter()
            .filter(|d| d["level"] == "error")
            .map(compiler_diagnostic)
            .collect::<Vec<_>>();
        if diagnostics.is_empty() {
            let mut summary = diagnostic(&build.code, build.message.clone());
            let stderr = build.stderr.trim();
            if !stderr.is_empty() {
                let tail = stderr
                    .char_indices()
                    .rev()
                    .nth(4095)
                    .map_or(stderr, |(index, _)| &stderr[index..]);
                summary["details"] = tail.into();
            }
            diagnostics.push(summary);
        }
        let code = match build.code.as_str() {
            "compiler.build"
            | "compiler.check"
            | "compiler.resolve"
            | "compiler.manifest"
            | "compiler.workspace"
            | "compiler.path"
            | "compiler.source_changed" => 1,
            _ => 3,
        };
        return (code, diagnostics);
    }
    (3, vec![diagnostic("command.failure", format!("{error:#}"))])
}

/// Normalizes a rustc JSON diagnostic to the engine diagnostic shape while
/// retaining compiler code, primary location and the rendered text.
pub fn compiler_diagnostic(message: &Value) -> Value {
    let primary = message["spans"]
        .as_array()
        .and_then(|spans| spans.iter().find(|span| span["is_primary"] == true));
    let code = message["code"]["code"]
        .as_str()
        .map(|code| format!("rust.{code}"))
        .unwrap_or_else(|| "rust.diagnostic".into());
    json!({
        "code": code,
        "severity": message["level"].as_str().unwrap_or("error"),
        "file": primary.and_then(|s| s["file_name"].as_str()).unwrap_or(""),
        "path": primary
            .map(|s| format!("{}:{}", s["line_start"], s["column_start"]))
            .unwrap_or_default(),
        "message": message["message"].as_str().unwrap_or(""),
        "rendered": message["rendered"].as_str().unwrap_or(""),
    })
}
