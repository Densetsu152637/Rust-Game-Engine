//! Compiler configuration shared by the project, run and MCP commands.

use super::errors::usage;
use crate::{
    artifacts,
    compiler::{CompileRequest, NativeCompiler},
};
use engine_core::{Result, anyhow::anyhow};
use engine_project::Project;
use std::path::{Path, PathBuf};

pub const NATIVE_MAIN: &str =
    "fn main() { std::process::exit(engine_tooling::runtime_entry::run(gameplay::configure)); }";

/// Generations retained per project cache by CLI builds, including the newest.
const RETAINED_GENERATIONS: usize = 3;

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
