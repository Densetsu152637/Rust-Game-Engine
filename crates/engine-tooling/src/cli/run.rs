//! `engine run`: compile a generation and run it headless or windowed.

use super::build::{compile_request, prune, rust_crate};
use super::errors::diagnostic;
use super::{Outcome, envelope};
use crate::{
    arguments::{parse_run, validate_actions},
    compiler::RunningGeneration,
};
use engine_core::{Result, anyhow::anyhow};
use engine_project::Project;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Read;
use std::sync::atomic::AtomicBool;

pub(super) fn run(arguments: &[OsString]) -> Result<Outcome> {
    let args = parse_run(arguments)?;
    let project = Project::load(&args.manifest)?;
    validate_actions(&project, &args)?;
    if rust_crate(&project)?.is_none() {
        return Err(engine_project::Error::new(
            "project.rust.missing",
            "Running requires the project's Rust gameplay crate",
        )
        .into());
    }
    let (mut compiler, request) = compile_request(&project)?;
    let artifact = compiler.compile(&request, &AtomicBool::new(false))?;
    prune(&request, &artifact.directory);
    let mut child =
        RunningGeneration::prepare_with_output(artifact, arguments, &AtomicBool::new(false), true)?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| anyhow!("Generation stdout was not captured"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(64 * 1024 * 1024).read_to_end(&mut bytes);
        result.map(|_| bytes)
    });
    child.start()?;
    let exit = child.wait(&AtomicBool::new(false))?;
    // Dropping the generation ends any descendant still holding the pipe.
    drop(child);
    let output = reader
        .join()
        .map_err(|_| anyhow!("Generation output reader panicked"))??;
    let output = String::from_utf8_lossy(&output);
    let mut result = None;
    for line in output.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(value) if value["schema"] == 1 && value["command"] == "run" => {
                if let Some(previous) = result.replace(line.to_owned()) {
                    eprintln!("{previous}");
                }
            }
            _ => eprintln!("{line}"),
        }
    }
    match result {
        Some(line) => {
            println!("{line}");
            Ok(Outcome::Exit(exit.code.unwrap_or(3)))
        }
        None => {
            println!(
                "{}",
                envelope(
                    false,
                    "run",
                    json!({"diagnostics": [diagnostic(
                        "runtime.output",
                        format!("Native generation exited with {:?} without a run result", exit.code),
                    )]}),
                )
            );
            Ok(Outcome::Exit(3))
        }
    }
}
