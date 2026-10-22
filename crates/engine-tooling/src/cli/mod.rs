//! `engine` command line. Every command except `mcp` prints exactly one JSON
//! envelope on stdout: `{"schema":1,"ok":bool,"command":string,"result":object}`.
//! Failures carry `result.diagnostics` entries with `code`, `severity`, `file`,
//! `path` and `message`. Exit codes: 0 success, 1 project/validation/build
//! failure, 2 invalid command arguments, 3 runtime or internal failure.

mod build;
mod editor;
mod errors;
mod mcp;
mod options;
mod project;
mod run;
mod scene;

pub use build::{NATIVE_MAIN, compile_request, compile_request_with};
pub(crate) use build::{prune, rust_crate};
pub use errors::{UsageError, compiler_diagnostic};

use engine_core::Result;
use errors::{classify, usage};
use serde_json::{Value, json};
use std::ffi::OsString;

const USAGE: &str = "Usage: engine <command> [--format json]\n  \
project init <template-manifest> <new-directory>\n  \
project validate <manifest> [--no-rust]\n  \
project assets <manifest>\n  \
project shaders <manifest>\n  \
project package <manifest> <new-directory> [--profile release|dev]\n  \
scene inspect <scene> --project-root <project-directory>\n  \
run <manifest> [--headless] [--ticks N] [--input action:tick]... [--user-data dir] [--scene-override file] [--render-backend primary|vulkan|dx12|metal|gl|all] [--screenshot file.png [--screenshot-size WxH]]\n  \
editor <manifest>\n  \
mcp <project-root> [--allow-write] [--allow-exec]";

/// Reserved stdout is one command result; native game logging belongs on stderr.
pub fn main(arguments: Vec<OsString>) -> i32 {
    if arguments.first().is_some_and(|v| v == "mcp") {
        return mcp::serve(&arguments[1..]);
    }
    let command = command_name(&arguments);
    match dispatch(&arguments) {
        Ok(Outcome::Exit(code)) => code,
        Ok(Outcome::Json(value)) => {
            println!("{}", envelope(true, &command, value));
            0
        }
        Err(error) => {
            let (code, diagnostics) = classify(&error);
            for diagnostic in &diagnostics {
                match diagnostic["rendered"].as_str() {
                    Some(rendered) => eprint!("{rendered}"),
                    None => eprintln!(
                        "{}: {}",
                        diagnostic["code"].as_str().unwrap_or("error"),
                        diagnostic["message"].as_str().unwrap_or_default()
                    ),
                }
            }
            println!(
                "{}",
                envelope(false, &command, json!({"diagnostics": diagnostics}))
            );
            code
        }
    }
}

fn envelope(ok: bool, command: &str, result: Value) -> Value {
    json!({"schema": 1, "ok": ok, "command": command, "result": result})
}

fn command_name(arguments: &[OsString]) -> String {
    let first = arguments.first().map(|v| v.to_string_lossy().into_owned());
    match first.as_deref() {
        Some(group @ ("project" | "scene")) => match arguments.get(1) {
            Some(action) if !action.to_string_lossy().starts_with("--") => {
                format!("{group} {}", action.to_string_lossy())
            }
            _ => group.to_owned(),
        },
        Some(name) if !name.starts_with("--") => name.to_owned(),
        _ => String::new(),
    }
}

enum Outcome {
    Json(Value),
    Exit(i32),
}

fn text(value: &OsString) -> Result<&str> {
    value
        .to_str()
        .ok_or_else(|| usage("Command names must be UTF-8"))
}

fn dispatch(arguments: &[OsString]) -> Result<Outcome> {
    let name = arguments.first().map(text).transpose()?.unwrap_or("");
    match name {
        "run" => run::run(&arguments[1..]),
        "project" => {
            let action = arguments.get(1).map(text).transpose()?.unwrap_or("");
            project::project(action, &arguments[2.min(arguments.len())..])
        }
        "scene" => {
            if arguments.get(1).map(text).transpose()? != Some("inspect") {
                return Err(usage(format!("Expected scene inspect\n{USAGE}")));
            }
            scene::scene_inspect(&arguments[2..])
        }
        "editor" => editor::editor(&arguments[1..]),
        "" => Err(usage(format!("Expected a command\n{USAGE}"))),
        other => Err(usage(format!("Unknown command: {other}\n{USAGE}"))),
    }
}
