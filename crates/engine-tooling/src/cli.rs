//! `engine` command line. Every command except `mcp` prints exactly one JSON
//! envelope on stdout: `{"schema":1,"ok":bool,"command":string,"result":object}`.
//! Failures carry `result.diagnostics` entries with `code`, `severity`, `file`,
//! `path` and `message`. Exit codes: 0 success, 1 project/validation/build
//! failure, 2 invalid command arguments, 3 runtime or internal failure.
use crate::{
    arguments::{parse_run, validate_actions},
    artifacts,
    compiler::{BuildFailure, CompileRequest, NativeCompiler, RunningGeneration},
};
use engine_core::{
    Result,
    anyhow::{Error, anyhow},
};
use engine_project::Project;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub const NATIVE_MAIN: &str =
    "fn main() { std::process::exit(engine_tooling::runtime_entry::run(gameplay::configure)); }";

/// Generations retained per project cache by CLI builds, including the newest.
const RETAINED_GENERATIONS: usize = 3;

const USAGE: &str = "Usage: engine <command> [--format json]\n  \
project init <template-manifest> <new-directory>\n  \
project validate <manifest> [--no-rust]\n  \
project assets <manifest>\n  \
project shaders <manifest>\n  \
project package <manifest> <new-directory> [--profile release|dev]\n  \
scene inspect <scene> --project-root <project-directory>\n  \
run <manifest> [--headless] [--ticks N] [--input action:tick]... [--user-data dir] [--scene-override file] [--screenshot file.png [--screenshot-size WxH]]\n  \
editor <manifest>\n  \
mcp <project-root> [--allow-write] [--allow-exec]";

const MCP_USAGE: &str = "Usage: engine mcp <project-root> [--allow-write] [--allow-exec]";

/// Parsed `engine mcp` arguments: the project root and the opt-in access flags.
struct McpArguments {
    root: PathBuf,
    allow_write: bool,
    allow_exec: bool,
}

fn parse_mcp(arguments: &[OsString]) -> std::result::Result<McpArguments, String> {
    let (mut root, mut write, mut exec) = (None, false, false);
    for argument in arguments {
        match argument.to_str() {
            Some("--allow-write") if !write => write = true,
            Some("--allow-exec") if !exec => exec = true,
            Some(flag @ ("--allow-write" | "--allow-exec")) => {
                return Err(format!("duplicate option {flag}"));
            }
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if root.is_none() => root = Some(PathBuf::from(argument)),
            _ => return Err("expected exactly one project root".into()),
        }
    }
    Ok(McpArguments {
        root: root.ok_or("expected a project root")?,
        allow_write: write,
        allow_exec: exec,
    })
}

/// Invalid command-line usage (exit code 2).
#[derive(Debug)]
pub struct UsageError(pub String);
impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UsageError {}

fn usage(message: impl Into<String>) -> Error {
    UsageError(message.into()).into()
}

/// Reserved stdout is one command result; native game logging belongs on stderr.
pub fn main(arguments: Vec<OsString>) -> i32 {
    if arguments.first().is_some_and(|v| v == "mcp") {
        let mcp = match parse_mcp(&arguments[1..]) {
            Ok(mcp) => mcp,
            Err(message) => {
                eprintln!("{message}\n{MCP_USAGE}");
                return 2;
            }
        };
        let mut config = engine_mcp::ServerConfig::new()
            .allow_write(mcp.allow_write)
            .allow_exec(mcp.allow_exec);
        if mcp.allow_exec {
            config = config.with_provider(crate::mcp_tools::ExecTools::new());
        }
        // Stdout carries JSON-RPC only; every diagnostic goes to stderr.
        return match engine_mcp::serve_with_config(
            &mcp.root,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            config,
        ) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("MCP: {error}");
                1
            }
        };
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

fn diagnostic(code: &str, message: impl Into<String>) -> Value {
    json!({"code": code, "severity": "error", "file": "", "path": "", "message": message.into()})
}

/// Maps an error to (exit code, diagnostics).
fn classify(error: &Error) -> (i32, Vec<Value>) {
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
        "run" => run(&arguments[1..]),
        "project" => {
            let action = arguments.get(1).map(text).transpose()?.unwrap_or("");
            project(action, &arguments[2.min(arguments.len())..])
        }
        "scene" => {
            if arguments.get(1).map(text).transpose()? != Some("inspect") {
                return Err(usage(format!("Expected scene inspect\n{USAGE}")));
            }
            scene_inspect(&arguments[2..])
        }
        "editor" => {
            let (args, _) = options(&arguments[1..], &[])?;
            if args.len() != 1 {
                return Err(usage("Usage: engine editor <manifest>"));
            }
            engine_editor::run(
                Path::new(&args[0]),
                Box::new(crate::play_host::NativePlayHost::new()),
            )?;
            Ok(Outcome::Json(json!({"closed": true})))
        }
        "" => Err(usage(format!("Expected a command\n{USAGE}"))),
        other => Err(usage(format!("Unknown command: {other}\n{USAGE}"))),
    }
}

fn project(action: &str, arguments: &[OsString]) -> Result<Outcome> {
    match action {
        "init" => {
            let (args, _) = options(arguments, &[])?;
            if args.len() != 2 {
                return Err(usage(
                    "Usage: engine project init <template-manifest> <new-directory>",
                ));
            }
            let manifest = artifacts::initialize_project(Path::new(&args[0]), Path::new(&args[1]))?;
            Ok(Outcome::Json(json!({
                "directory": Path::new(&args[1]),
                "manifest": manifest,
            })))
        }
        "validate" => {
            let (args, flags) = options(arguments, &["--no-rust"])?;
            if args.len() != 1 {
                return Err(usage(
                    "Usage: engine project validate <manifest> [--no-rust]",
                ));
            }
            let project = Project::load(&args[0])?;
            let rust = if flags.contains_key("--no-rust") {
                Value::Null
            } else {
                match rust_crate(&project)? {
                    None => Value::Null,
                    Some(_) => {
                        let (compiler, request) = compile_request(&project)?;
                        let report = compiler.check(&request, &AtomicBool::new(false))?;
                        json!({
                            "manifest": request.project_manifest,
                            "fingerprint": report.fingerprint,
                            "diagnostics": report.diagnostics.iter().map(compiler_diagnostic).collect::<Vec<_>>(),
                        })
                    }
                }
            };
            let mut scenes = vec![json!({"path": project.startup_scene, "id": project.scene.id})];
            for asset in project.assets.values().filter(|a| a.kind == "scene") {
                scenes.push(json!({"path": asset.path, "asset": asset.id}));
            }
            Ok(Outcome::Json(json!({
                "project": project.name,
                "manifest": project.manifest,
                "assets": project.assets.len(),
                "scenes": scenes,
                "rust": rust,
                "diagnostics": [],
            })))
        }
        "assets" => {
            let project = single_manifest(arguments, "assets")?;
            let index = artifacts::asset_index(&project)?;
            let path = artifacts::write_asset_index(&project)?;
            Ok(Outcome::Json(
                json!({"index": path, "assets": index.assets}),
            ))
        }
        "shaders" => {
            let project = single_manifest(arguments, "shaders")?;
            let imported = artifacts::import_shaders(&project)?;
            Ok(Outcome::Json(serde_json::to_value(imported)?))
        }
        "package" => {
            let (args, flags) = options(arguments, &["--profile"])?;
            if args.len() != 2 {
                return Err(usage(
                    "Usage: engine project package <manifest> <new-directory> [--profile release|dev]",
                ));
            }
            let release = match flags.get("--profile").map(String::as_str) {
                None | Some("release") => true,
                Some("dev") => false,
                Some(other) => return Err(usage(format!("Unknown profile: {other}"))),
            };
            let project = Project::load(&args[0])?;
            let destination = Path::new(&args[1]);
            // Reject unusable destinations before spending time compiling.
            artifacts::check_package_destination(&project, destination)?;
            if rust_crate(&project)?.is_none() {
                return Err(engine_project::Error::new(
                    "project.rust.missing",
                    "Packaging requires the project's Rust gameplay crate",
                )
                .into());
            }
            let (mut compiler, mut request) = compile_request(&project)?;
            request.release = release;
            let generation = compiler.compile(&request, &AtomicBool::new(false))?;
            prune(&request, &generation.directory);
            let path = artifacts::package_project(
                &project,
                &generation,
                destination,
                &request.engine_root,
            )?;
            Ok(Outcome::Json(json!({
                "directory": path,
                "generation": generation.generation,
                "fingerprint": generation.fingerprint,
                "profile": if release { "release" } else { "dev" },
                "launchers": ["run.cmd", "run.sh"],
            })))
        }
        "" => Err(usage(format!("Expected a project command\n{USAGE}"))),
        other => Err(usage(format!("Unknown project command: {other}\n{USAGE}"))),
    }
}

fn single_manifest(arguments: &[OsString], action: &str) -> Result<Project> {
    let (args, _) = options(arguments, &[])?;
    if args.len() != 1 {
        return Err(usage(format!("Usage: engine project {action} <manifest>")));
    }
    Ok(Project::load(&args[0])?)
}

fn scene_inspect(arguments: &[OsString]) -> Result<Outcome> {
    let (args, flags) = options(arguments, &["--project-root"])?;
    let (Some(root), [scene]) = (flags.get("--project-root"), args.as_slice()) else {
        return Err(usage(
            "Usage: engine scene inspect <scene> --project-root <project-directory>",
        ));
    };
    let project = Project::load(Path::new(root).join("project.json"))?;
    let input = Path::new(scene);
    let relative = if input.is_absolute() {
        let canonical = input.canonicalize().map_err(|e| {
            engine_project::Error::new("project.asset.missing", e.to_string()).at(input, "")
        })?;
        canonical
            .strip_prefix(&project.root)
            .map_err(|_| {
                engine_project::Error::new(
                    "project.path.outside_root",
                    "Scene must be inside the project root",
                )
                .at(input, "")
            })?
            .to_path_buf()
    } else {
        input.to_path_buf()
    };
    let scene = engine_project::load_scene(&project.root, &relative)?;
    project.validate_scene(&scene)?;
    Ok(Outcome::Json(json!({
        "revision": scene.revision(),
        "entities": scene.entities.len(),
        "scene": scene,
    })))
}

fn run(arguments: &[OsString]) -> Result<Outcome> {
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

/// Parses positional arguments, `--format json` and the listed options. Options
/// whose name is listed take a value, except boolean `--no-*` flags.
fn options(
    arguments: &[OsString],
    allowed: &[&str],
) -> Result<(Vec<OsString>, std::collections::BTreeMap<String, String>)> {
    let mut positional = Vec::new();
    let mut flags = std::collections::BTreeMap::new();
    let mut format = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let Some(name) = argument.to_str().filter(|v| v.starts_with("--")) else {
            positional.push(argument.clone());
            index += 1;
            continue;
        };
        if name == "--format" {
            if format || arguments.get(index + 1).is_none_or(|v| v != "json") {
                return Err(usage("Expected one --format json"));
            }
            format = true;
            index += 2;
            continue;
        }
        if !allowed.contains(&name) {
            return Err(usage(format!("Unknown option: {name}")));
        }
        if flags.contains_key(name) {
            return Err(usage(format!("Duplicate option: {name}")));
        }
        if name.starts_with("--no-") {
            flags.insert(name.to_owned(), String::new());
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .and_then(|v| v.to_str())
            .ok_or_else(|| usage(format!("Missing UTF-8 value for {name}")))?;
        flags.insert(name.to_owned(), value.to_owned());
        index += 2;
    }
    Ok((positional, flags))
}

/// The project's Rust crate, or `None` for a content-only project. Script assets
/// require a crate because native gameplay is compiled from it.
pub(crate) fn rust_crate(project: &Project) -> Result<Option<PathBuf>> {
    match project.rust_manifest() {
        Ok(path) => Ok(Some(path)),
        Err(_) if !project.assets.values().any(|a| a.kind == "script") => Ok(None),
        Err(error) => Err(engine_project::Error::new(
            "project.rust.missing",
            format!("Script assets require the Rust gameplay crate: {error}"),
        )
        .at(&project.rust_manifest, "rust.manifest")
        .into()),
    }
}

pub(crate) fn prune(request: &CompileRequest, current: &Path) {
    if let Err(error) =
        artifacts::prune_generations(&request.cache_root, current, RETAINED_GENERATIONS)
    {
        eprintln!("generation cache pruning skipped: {error}");
    }
}

/// Compiler configuration for a project. `ENGINE_CARGO`/`ENGINE_RUSTC` select
/// the toolchain and `ENGINE_NATIVE_TARGET_DIR` shares dependency output.
pub fn compile_request(project: &Project) -> Result<(NativeCompiler, CompileRequest)> {
    compile_request_with(
        project,
        std::env::var_os("ENGINE_NATIVE_TARGET_DIR").map(PathBuf::from),
    )
}

/// As [`compile_request`] with an explicit shared Cargo target directory.
pub fn compile_request_with(
    project: &Project,
    target_dir: Option<PathBuf>,
) -> Result<(NativeCompiler, CompileRequest)> {
    let engine_root = locate_engine()?;
    let cargo = std::env::var_os("ENGINE_CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO")));
    let rustc = std::env::var_os("ENGINE_RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            cargo
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(if cfg!(windows) { "rustc.exe" } else { "rustc" })
        });
    let request = CompileRequest {
        engine_root,
        project_manifest: project.rust_manifest()?,
        cache_root: artifacts::confined_output_directory(
            &project.root,
            Path::new(".derived/native"),
        )?,
        main_source: NATIVE_MAIN.into(),
        features: vec![],
        target: None,
        release: false,
        offline: false,
        target_dir,
    };
    Ok((NativeCompiler::new(cargo, rustc), request))
}

fn locate_engine() -> Result<PathBuf> {
    let is_engine = |root: &Path| root.join("crates/engine-core/Cargo.toml").is_file();
    if let Some(root) = std::env::var_os("ENGINE_SOURCE_ROOT") {
        let root = PathBuf::from(root);
        if !is_engine(&root) {
            return Err(usage("ENGINE_SOURCE_ROOT is not an engine checkout"));
        }
        return Ok(root.canonicalize()?);
    }
    let compiled = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if is_engine(&compiled) {
        return Ok(compiled.canonicalize()?);
    }
    let executable = std::env::current_exe()?;
    for parent in executable.ancestors().take(8) {
        if is_engine(parent) {
            return Ok(parent.canonicalize()?);
        }
    }
    Err(anyhow!(
        "Cannot locate engine sources; set ENGINE_SOURCE_ROOT"
    ))
}
