//! The native compiler: prepares, builds and checks generated workspaces.

use super::diagnostics::build_outcome;
use super::fingerprint::fingerprint;
use super::process::run_compiler;
use super::workspace::*;
use super::{
    BuildArtifact, BuildFailure, CheckReport, CompileRequest, GENERATION_SEQUENCE, cancelled,
    failure, io_error,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
/// Compiler paths are explicit so a selected toolchain cannot silently become
/// another compiler via a changed PATH during a reload.
pub struct NativeCompiler {
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    pub(super) last_good: Option<BuildArtifact>,
}

/// A generated single-package workspace whose dependency graph has been resolved.
pub(super) struct PreparedGeneration {
    pub(super) cache: PathBuf,
    pub(super) generation: String,
    pub(super) directory: PathBuf,
    pub(super) binary_name: String,
    pub(super) roots: Vec<PathBuf>,
    pub(super) neutral: PathBuf,
    pub(super) identity: String,
    pub(super) fingerprint: String,
}

impl NativeCompiler {
    pub fn new(cargo: impl Into<PathBuf>, rustc: impl Into<PathBuf>) -> Self {
        Self {
            cargo: cargo.into(),
            rustc: rustc.into(),
            last_good: None,
        }
    }

    pub fn last_good(&self) -> Option<&BuildArtifact> {
        self.last_good.as_ref()
    }

    /// A failed or cancelled build never changes the last successful artifact.
    /// Callers may continue running the previous child while this blocks.
    pub fn compile(
        &mut self,
        request: &CompileRequest,
        cancel: &AtomicBool,
    ) -> Result<BuildArtifact, BuildFailure> {
        let started = Instant::now();
        tracing::info!(
            manifest = %request.project_manifest.display(),
            release = request.release,
            offline = request.offline,
            "native build started"
        );
        let prepared = self.prepare(request, cancel, "").inspect_err(|error| {
            tracing::warn!(code = %error.code, "native build could not prepare a generation");
        })?;
        let directory = prepared.directory.clone();
        tracing::debug!(generation = %prepared.generation, fingerprint = %prepared.fingerprint, "generation prepared");
        let result = self.build(prepared, request, cancel);
        let duration_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(artifact) => tracing::info!(
                generation = %artifact.generation,
                fingerprint = %artifact.fingerprint,
                duration_ms,
                "native build finished"
            ),
            Err(error) => tracing::warn!(
                code = %error.code,
                cancelled = error.cancelled,
                duration_ms,
                "native build failed"
            ),
        }
        if result.is_err() {
            // Failed candidates leave no generation; the last good one is untouched.
            let _ = fs::remove_dir_all(directory);
        }
        result
    }

    fn build(
        &mut self,
        prepared: PreparedGeneration,
        request: &CompileRequest,
        cancel: &AtomicBool,
    ) -> Result<BuildArtifact, BuildFailure> {
        let output = self.run_cargo(&prepared, request, "build", cancel)?;
        let mut diagnostics = Vec::new();
        let mut executable = None;
        for line in output.stdout.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                match value["reason"].as_str() {
                    Some("compiler-message") => diagnostics.push(value["message"].clone()),
                    Some("compiler-artifact")
                        if value["target"]["name"] == prepared.binary_name.as_str() =>
                    {
                        executable = value["executable"].as_str().map(PathBuf::from);
                    }
                    _ => {}
                }
            }
        }
        build_outcome(&output, &diagnostics, "compiler.build")?;
        self.verify_unchanged(request, &prepared)?;
        let built = executable.ok_or_else(|| {
            failure(
                "compiler.artifact",
                "Cargo succeeded without a runtime executable artifact",
            )
        })?;
        // Cargo uses a shared target cache; copy the binary out before another
        // build can replace it or Windows locks a running generation's image.
        let executable = prepared.directory.join(if cfg!(windows) {
            "runtime.exe"
        } else {
            "runtime"
        });
        fs::copy(built, &executable).map_err(io_error)?;
        let artifact = BuildArtifact {
            fingerprint: prepared.fingerprint,
            generation: prepared.generation,
            executable,
            directory: prepared.directory,
            diagnostics,
        };
        fs::write(
            artifact.directory.join("generation.json"),
            serde_json::to_vec_pretty(&artifact).map_err(|e| failure("compiler.metadata", e))?,
        )
        .map_err(io_error)?;
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        self.last_good = Some(artifact.clone());
        Ok(artifact)
    }

    /// Compile-only validation (`cargo check`) of the exact generation graph.
    /// It never produces or replaces a generation; its workspace is removed.
    pub fn check(
        &self,
        request: &CompileRequest,
        cancel: &AtomicBool,
    ) -> Result<CheckReport, BuildFailure> {
        let started = Instant::now();
        tracing::info!(manifest = %request.project_manifest.display(), "native check started");
        let prepared = self.prepare(request, cancel, "check-")?;
        let result: Result<CheckReport, BuildFailure> = (|| {
            let output = self.run_cargo(&prepared, request, "check", cancel)?;
            let diagnostics = output
                .stdout
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|value| value["reason"] == "compiler-message")
                .map(|value| value["message"].clone())
                .collect::<Vec<_>>();
            build_outcome(&output, &diagnostics, "compiler.check")?;
            self.verify_unchanged(request, &prepared)?;
            Ok(CheckReport {
                fingerprint: prepared.fingerprint.clone(),
                diagnostics,
            })
        })();
        let _ = fs::remove_dir_all(&prepared.directory);
        let duration_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(_) => tracing::info!(duration_ms, "native check finished"),
            Err(error) => {
                tracing::warn!(code = %error.code, duration_ms, "native check failed");
            }
        }
        result
    }

    fn prepare(
        &self,
        request: &CompileRequest,
        cancel: &AtomicBool,
        prefix: &str,
    ) -> Result<PreparedGeneration, BuildFailure> {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let engine_root = request.engine_root.canonicalize().map_err(io_error)?;
        let manifest = request.project_manifest.canonicalize().map_err(io_error)?;
        let project_root = manifest
            .parent()
            .ok_or_else(|| failure("compiler.manifest", "Manifest has no parent"))?;
        let package_name = package_name(&manifest)?;
        let neutral = self.neutral_directory()?;
        let identity = self.identity(&neutral, cancel)?;
        fs::create_dir_all(&request.cache_root).map_err(io_error)?;
        let cache = request.cache_root.canonicalize().map_err(io_error)?;
        if cache == engine_root || cache == project_root {
            return Err(failure(
                "compiler.cache",
                "Cache must be a separate generated directory",
            ));
        }
        let generation = format!(
            "{prefix}{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let directory = cache.join(&generation);
        fs::create_dir(&directory).map_err(io_error)?;
        let prepared = (|| {
            fs::create_dir(directory.join("src")).map_err(io_error)?;
            // Unique per generation so builds sharing a target directory never
            // race on one uplifted executable. rustc repeats the crate name in
            // every object file name (`<name>.<name>.<hash>-cgu.N.rcgu.o` when
            // incremental compilation is off, as in release builds), so the full
            // generation ID would push deep project paths past MAX_PATH, which
            // the MSVC linker reports as LNK1181. A short digest keeps it unique.
            let digest = format!("{:x}", Sha256::digest(generation.as_bytes()));
            let binary_name = format!("runtime-{}", &digest[..12]);
            fs::write(
                directory.join("Cargo.toml"),
                generated_manifest(
                    &package_name,
                    project_root,
                    &engine_root,
                    &request.features,
                    &binary_name,
                )?,
            )
            .map_err(io_error)?;
            fs::write(directory.join("src/entry.rs"), &request.main_source).map_err(io_error)?;
            fs::write(directory.join("src/main.rs"), STARTUP_WRAPPER).map_err(io_error)?;
            // Seed dependency versions without ever changing the authored lockfile.
            for lock in [
                project_root.join("Cargo.lock"),
                engine_root.join("Cargo.lock"),
            ] {
                if lock.is_file() {
                    fs::copy(lock, directory.join("Cargo.lock")).map_err(io_error)?;
                    break;
                }
            }
            let mut resolution = self.cargo_command(&neutral, request);
            resolution
                .arg("metadata")
                .arg("--format-version=1")
                .arg("--manifest-path")
                .arg(directory.join("Cargo.toml"));
            let platform = request
                .target
                .as_deref()
                .or_else(|| {
                    identity
                        .lines()
                        .find_map(|line| line.strip_prefix("host: "))
                })
                .ok_or_else(|| {
                    failure("compiler.toolchain", "rustc did not report its host target")
                })?;
            resolution.arg("--filter-platform").arg(platform);
            if request.offline {
                resolution.arg("--offline");
            }
            let resolved = run_compiler(resolution, cancel)?;
            if resolved.cancelled {
                return Err(cancelled());
            }
            if !resolved.status.success() {
                return Err(BuildFailure {
                    code: "compiler.resolve".into(),
                    message: "Generation dependency resolution failed".into(),
                    diagnostics: vec![],
                    stderr: resolved.stderr,
                    exit_code: resolved.status.code(),
                    cancelled: false,
                });
            }
            let metadata: serde_json::Value = serde_json::from_str(&resolved.stdout)
                .map_err(|e| failure("compiler.metadata", e))?;
            fs::write(directory.join("cargo-metadata.json"), &resolved.stdout).map_err(io_error)?;
            let roots = local_package_roots(&metadata, &directory)?;
            let fingerprint = fingerprint(request, &roots, &directory, &identity, &neutral)?;
            Ok((binary_name, roots, fingerprint))
        })();
        match prepared {
            Ok((binary_name, roots, fingerprint)) => Ok(PreparedGeneration {
                cache,
                generation,
                directory,
                binary_name,
                roots,
                neutral,
                identity,
                fingerprint,
            }),
            Err(error) => {
                // An unresolvable candidate leaves no half-written generation.
                let _ = fs::remove_dir_all(&directory);
                Err(error)
            }
        }
    }
    fn verify_unchanged(
        &self,
        request: &CompileRequest,
        prepared: &PreparedGeneration,
    ) -> Result<(), BuildFailure> {
        if fingerprint(
            request,
            &prepared.roots,
            &prepared.directory,
            &prepared.identity,
            &prepared.neutral,
        )? != prepared.fingerprint
        {
            return Err(failure(
                "compiler.source_changed",
                "Sources changed during compilation; previous generation retained",
            ));
        }
        Ok(())
    }
}
