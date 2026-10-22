//! Compile external Rust gameplay together with the engine into unique native
//! executable generations. No Rust ABI crosses a dynamically loaded boundary.

use process_wrap::std::{ChildWrapper, CommandWrap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Every field except `target_dir` affects generation identity. `main_source` is
/// the host's real native entry point and may invoke unrestricted public engine/game
/// APIs. `target_dir` only shares Cargo's dependency output between caches; it
/// defaults to `<cache_root>/target`.
#[derive(Clone, Debug)]
pub struct CompileRequest {
    pub engine_root: PathBuf,
    pub project_manifest: PathBuf,
    pub cache_root: PathBuf,
    pub main_source: String,
    pub features: Vec<String>,
    pub target: Option<String>,
    pub release: bool,
    pub offline: bool,
    pub target_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub fingerprint: String,
    pub generation: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub diagnostics: Vec<serde_json::Value>,
}

/// Result of compile-only validation. No executable or generation is retained.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub fingerprint: String,
    pub diagnostics: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildFailure {
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<serde_json::Value>,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
}

impl std::fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for BuildFailure {}

fn failure(code: &str, message: impl ToString) -> BuildFailure {
    BuildFailure {
        code: code.to_owned(),
        message: message.to_string(),
        diagnostics: Vec::new(),
        stderr: String::new(),
        exit_code: None,
        cancelled: false,
    }
}

fn io_error(error: std::io::Error) -> BuildFailure {
    failure("compiler.io", error)
}

/// Compiler paths are explicit so a selected toolchain cannot silently become
/// another compiler via a changed PATH during a reload.
pub struct NativeCompiler {
    pub cargo: PathBuf,
    pub rustc: PathBuf,
    last_good: Option<BuildArtifact>,
}

/// A generated single-package workspace whose dependency graph has been resolved.
struct PreparedGeneration {
    cache: PathBuf,
    generation: String,
    directory: PathBuf,
    binary_name: String,
    roots: Vec<PathBuf>,
    neutral: PathBuf,
    identity: String,
    fingerprint: String,
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
        let prepared = self.prepare(request, cancel, "")?;
        let directory = prepared.directory.clone();
        let result = self.build(prepared, request, cancel);
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
        let prepared = self.prepare(request, cancel, "check-")?;
        let result = (|| {
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
        let identity = self.identity(&neutral)?;
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

    fn run_cargo(
        &self,
        prepared: &PreparedGeneration,
        request: &CompileRequest,
        subcommand: &str,
        cancel: &AtomicBool,
    ) -> Result<CompilerOutput, BuildFailure> {
        let target_dir = match &request.target_dir {
            Some(directory) => {
                fs::create_dir_all(directory).map_err(io_error)?;
                directory.canonicalize().map_err(io_error)?
            }
            None => prepared.cache.join("target"),
        };
        let mut command = self.cargo_command(&prepared.neutral, request);
        command
            .arg(subcommand)
            .arg("--manifest-path")
            .arg(prepared.directory.join("Cargo.toml"))
            .arg("--message-format=json")
            .arg("--locked")
            .arg("--target-dir")
            .arg(target_dir);
        if request.offline {
            command.arg("--offline");
        }
        if request.release {
            command.arg("--release");
        }
        if let Some(target) = &request.target {
            command.arg("--target").arg(target);
        }
        run_compiler(command, cancel)
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

    /// Cargo discovers `.cargo/config*` from its working directory upwards. Using
    /// the selected toolchain's own directory keeps configuration above the game
    /// package, project or generated cache out of the build. Rustup proxies also
    /// resolve their toolchain from this directory rather than from the project.
    fn neutral_directory(&self) -> Result<PathBuf, BuildFailure> {
        self.cargo
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .and_then(|parent| parent.canonicalize().ok())
            .or_else(cargo_home)
            .filter(|directory| directory.is_dir())
            .ok_or_else(|| {
                failure(
                    "compiler.toolchain",
                    "Cargo must be an explicit executable path or CARGO_HOME must exist",
                )
            })
    }

    fn identity(&self, neutral: &Path) -> Result<String, BuildFailure> {
        let mut identity = String::new();
        for (program, argument) in [(&self.rustc, "-vV"), (&self.cargo, "--version")] {
            let output = Command::new(program)
                .arg(argument)
                .current_dir(neutral)
                .output()
                .map_err(io_error)?;
            if !output.status.success() {
                return Err(failure(
                    "compiler.toolchain",
                    String::from_utf8_lossy(&output.stderr),
                ));
            }
            identity.push_str(&program.to_string_lossy());
            identity.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        Ok(identity)
    }

    fn cargo_command(&self, neutral: &Path, request: &CompileRequest) -> Command {
        let mut command = Command::new(&self.cargo);
        command.env("RUSTC", &self.rustc).current_dir(neutral);
        if let Some(target) = &request.target {
            command.env("CARGO_BUILD_TARGET", target);
        }
        command
    }
}

fn build_outcome(
    output: &CompilerOutput,
    diagnostics: &[serde_json::Value],
    code: &str,
) -> Result<(), BuildFailure> {
    if output.status.success() && !output.cancelled {
        return Ok(());
    }
    Err(BuildFailure {
        code: if output.cancelled {
            "compiler.cancelled"
        } else {
            code
        }
        .into(),
        message: if output.cancelled {
            "Compilation cancelled"
        } else {
            "Native Rust compilation failed"
        }
        .into(),
        diagnostics: diagnostics.to_vec(),
        stderr: output.stderr.clone(),
        exit_code: output.status.code(),
        cancelled: output.cancelled,
    })
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .and_then(|path| path.canonicalize().ok())
}

const STARTUP_WRAPPER: &str = r#"
mod native_entry {
    include!("entry.rs");
    pub fn invoke() { main(); }
}
fn main() {
    if let Some(ready) = std::env::var_os("ENGINE_GENERATION_READY_FILE") {
        if let Err(error) = std::fs::write(ready, b"ready") { eprintln!("generation readiness: {error}"); std::process::exit(3); }
        let mut start = String::new();
        if std::io::stdin().read_line(&mut start).is_err() || start.trim() != "start" { std::process::exit(3); }
    }
    native_entry::invoke();
}
"#;

fn cancelled() -> BuildFailure {
    let mut result = failure("compiler.cancelled", "Compilation cancelled");
    result.cancelled = true;
    result
}

fn package_name(manifest: &Path) -> Result<String, BuildFailure> {
    let value: toml::Value = fs::read_to_string(manifest)
        .map_err(io_error)?
        .parse()
        .map_err(|e| failure("compiler.manifest", e))?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                "compiler.manifest",
                "External gameplay manifest requires [package].name",
            )
        })
}

fn generated_manifest(
    package: &str,
    project: &Path,
    engine: &Path,
    features: &[String],
    binary_name: &str,
) -> Result<String, BuildFailure> {
    let mut dependencies = toml::map::Map::new();
    let mut game = toml::map::Map::new();
    game.insert("package".into(), package.into());
    game.insert("path".into(), project.to_string_lossy().as_ref().into());
    game.insert(
        "features".into(),
        toml::Value::Array(features.iter().cloned().map(toml::Value::String).collect()),
    );
    dependencies.insert("gameplay".into(), toml::Value::Table(game));
    for name in [
        "engine-core",
        "engine-project",
        "engine-desktop",
        "engine-tooling",
    ] {
        let path = engine.join("crates").join(name);
        if path.join("Cargo.toml").is_file() {
            let mut dependency = toml::map::Map::new();
            dependency.insert("path".into(), path.to_string_lossy().as_ref().into());
            dependencies.insert(name.into(), toml::Value::Table(dependency));
        }
    }
    let mut manifest = toml::map::Map::new();
    let mut package = toml::map::Map::new();
    package.insert("name".into(), "engine-runtime-generation".into());
    package.insert("version".into(), "0.0.0".into());
    package.insert("edition".into(), "2024".into());
    manifest.insert("package".into(), toml::Value::Table(package));
    let mut binary = toml::map::Map::new();
    binary.insert("name".into(), binary_name.into());
    binary.insert("path".into(), "src/main.rs".into());
    manifest.insert(
        "bin".into(),
        toml::Value::Array(vec![toml::Value::Table(binary)]),
    );
    manifest.insert("workspace".into(), toml::Value::Table(Default::default()));
    manifest.insert("dependencies".into(), toml::Value::Table(dependencies));
    toml::to_string(&manifest).map_err(|e| failure("compiler.manifest", e))
}

fn local_package_roots(
    metadata: &serde_json::Value,
    generation: &Path,
) -> Result<Vec<PathBuf>, BuildFailure> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| failure("compiler.metadata", "Cargo metadata has no packages"))?;
    let mut roots = Vec::new();
    for package in packages {
        if !package["source"].is_null() {
            continue;
        }
        let manifest = package["manifest_path"]
            .as_str()
            .ok_or_else(|| failure("compiler.metadata", "Package has no manifest path"))?;
        let root = Path::new(manifest)
            .parent()
            .ok_or_else(|| failure("compiler.metadata", "Package manifest has no parent"))?
            .canonicalize()
            .map_err(io_error)?;
        if root != generation {
            roots.push(root);
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Environment that Cargo/rustc/build scripts treat as build configuration.
const FINGERPRINT_VARIABLES: [&str; 11] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "CARGO_INCREMENTAL",
];
const FINGERPRINT_PREFIXES: [&str; 5] = [
    "CARGO_BUILD_",
    "CARGO_PROFILE_",
    "CARGO_TARGET_",
    "CARGO_ENCODED_",
    "CARGO_UNSTABLE_",
];

fn fingerprint_environment() -> BTreeMap<String, OsString> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value)))
        .filter(|(name, _)| {
            FINGERPRINT_VARIABLES.contains(&name.as_str())
                || (FINGERPRINT_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                    // Output locations do not change compiled behavior.
                    && name != "CARGO_TARGET_DIR"
                    && name != "CARGO_TARGET_TMPDIR")
        })
        .collect()
}

fn fingerprint(
    request: &CompileRequest,
    roots: &[PathBuf],
    generation: &Path,
    identity: &str,
    neutral: &Path,
) -> Result<String, BuildFailure> {
    let mut hash = Sha256::new();
    fn field(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    field(&mut hash, b"engine-native-generation-v2");
    field(&mut hash, identity.as_bytes());
    field(&mut hash, request.main_source.as_bytes());
    field(&mut hash, STARTUP_WRAPPER.as_bytes());
    field(
        &mut hash,
        &fs::read(generation.join("Cargo.lock")).map_err(io_error)?,
    );
    field(
        &mut hash,
        format!(
            "{:?}{:?}{}{}",
            request.features, request.target, request.release, request.offline
        )
        .as_bytes(),
    );
    for (name, value) in fingerprint_environment() {
        field(&mut hash, name.as_bytes());
        field(&mut hash, value.to_string_lossy().as_bytes());
    }
    let cache = request.cache_root.canonicalize().map_err(io_error)?;
    for root in roots {
        field(&mut hash, root.to_string_lossy().as_bytes());
        let mut files = BTreeMap::new();
        collect_sources(root, root, &cache, &mut files)?;
        for (relative, path) in files {
            field(&mut hash, relative.as_bytes());
            field(&mut hash, &fs::read(path).map_err(io_error)?);
        }
    }
    for path in configuration_inputs(neutral, roots)? {
        field(&mut hash, path.to_string_lossy().as_bytes());
        field(&mut hash, &fs::read(path).map_err(io_error)?);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Exactly the out-of-package files Cargo reads for this generation: config
/// discovered from the neutral working directory and Cargo home, plus the
/// workspace root of each local package that inherits workspace fields. Unrelated
/// ancestor workspaces/configs of the game, project or cache are not inputs.
fn configuration_inputs(
    neutral: &Path,
    roots: &[PathBuf],
) -> Result<std::collections::BTreeSet<PathBuf>, BuildFailure> {
    let mut inputs = std::collections::BTreeSet::new();
    let mut directories = neutral
        .ancestors()
        .map(|ancestor| ancestor.join(".cargo"))
        .collect::<Vec<_>>();
    directories.extend(cargo_home());
    for directory in directories {
        for name in ["config", "config.toml"] {
            let path = directory.join(name);
            if path.is_file() {
                inputs.insert(path.canonicalize().map_err(io_error)?);
            }
        }
    }
    for root in roots {
        if let Some(workspace) = inherited_workspace(root)? {
            inputs.insert(workspace);
        }
    }
    Ok(inputs)
}

/// The workspace manifest a non-root package inherits from, mirroring Cargo's
/// lookup: an explicit `package.workspace`, otherwise the nearest ancestor
/// manifest declaring `[workspace]`. Packages that are their own workspace root
/// or inherit nothing have no external workspace input.
fn inherited_workspace(root: &Path) -> Result<Option<PathBuf>, BuildFailure> {
    let manifest = parse_manifest(&root.join("Cargo.toml"))?;
    if manifest.get("workspace").is_some() {
        return Ok(None);
    }
    if let Some(explicit) = manifest
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str)
    {
        let path = root.join(explicit).join("Cargo.toml");
        return path.canonicalize().map(Some).map_err(|_| {
            failure(
                "compiler.workspace",
                "package.workspace manifest is missing",
            )
        });
    }
    if !inherits_workspace(&manifest) {
        return Ok(None);
    }
    for ancestor in root.ancestors().skip(1) {
        let candidate = ancestor.join("Cargo.toml");
        if candidate.is_file() && parse_manifest(&candidate)?.get("workspace").is_some() {
            return candidate.canonicalize().map(Some).map_err(io_error);
        }
    }
    Err(failure(
        "compiler.workspace",
        format!(
            "{} inherits workspace fields without a workspace root",
            root.display()
        ),
    ))
}

fn parse_manifest(path: &Path) -> Result<toml::Value, BuildFailure> {
    fs::read_to_string(path)
        .map_err(io_error)?
        .parse()
        .map_err(|e| failure("compiler.manifest", e))
}

fn inherits_workspace(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => table.iter().any(|(key, value)| {
            (key == "workspace" && value.as_bool() == Some(true)) || inherits_workspace(value)
        }),
        toml::Value::Array(items) => items.iter().any(inherits_workspace),
        _ => false,
    }
}

fn collect_sources(
    root: &Path,
    directory: &Path,
    cache: &Path,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<(), BuildFailure> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        if path == cache {
            continue;
        }
        let name = entry.file_name();
        if [
            ".git", "target", "build", "dist", "release", ".derived", ".idea", ".vs",
        ]
        .iter()
        .any(|skip| name == *skip)
            || name.to_string_lossy().starts_with("cmake-build-")
        {
            continue;
        }
        let canonical = path.canonicalize().map_err(io_error)?;
        if !canonical.starts_with(root) {
            return Err(failure(
                "compiler.path",
                format!("Source link escapes root: {}", path.display()),
            ));
        }
        let metadata = entry.path().symlink_metadata().map_err(io_error)?;
        #[cfg(windows)]
        let linked = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let linked = metadata.file_type().is_symlink();
        if linked {
            // Reject directory aliases rather than recurse through cycles or
            // miss changes through an external link.
            return Err(failure(
                "compiler.path",
                format!("Source symlinks are unsupported: {}", path.display()),
            ));
        }
        if canonical.is_dir() {
            collect_sources(root, &canonical, cache, files)?;
        } else if canonical.is_file() {
            files.insert(
                path.strip_prefix(root)
                    .map_err(|e| failure("compiler.path", e))?
                    .to_string_lossy()
                    .replace('\\', "/"),
                canonical,
            );
        }
    }
    Ok(())
}

struct CompilerOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    cancelled: bool,
}

fn run_compiler(mut command: Command, cancel: &AtomicBool) -> Result<CompilerOutput, BuildFailure> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = spawn_owned(command)?;
    let stdout = child.stdout().take().expect("piped stdout");
    let stderr = child.stderr().take().expect("piped stderr");
    let (send, receive) = mpsc::channel();
    let stdout_send = send.clone();
    let out_thread = thread::spawn(move || {
        let _ = stdout_send.send((true, read_bounded(stdout)));
    });
    let err_thread = thread::spawn(move || {
        let _ = send.send((false, read_bounded(stderr)));
    });
    let mut was_cancelled = false;
    let mut status = None;
    let mut drain_deadline = None;
    let mut leaked_output = false;
    let mut received = 0;
    let mut stdout = String::new();
    let mut stderr = String::new();
    while status.is_none() || received < 2 {
        if cancel.load(Ordering::Acquire) && !was_cancelled {
            terminate_tree(&mut child)?;
            was_cancelled = true;
            if status.is_none() {
                status = Some(reap(&mut child)?);
            }
        }
        if status.is_none()
            && let Some(exit) = child.try_wait().map_err(io_error)?
        {
            status = Some(exit);
            drain_deadline = Some(std::time::Instant::now() + Duration::from_secs(2));
        }
        match receive.recv_timeout(Duration::from_millis(20)) {
            Ok((is_stdout, output)) => {
                let output = output.map_err(io_error)?;
                if is_stdout {
                    stdout = output;
                } else {
                    stderr = output;
                }
                received += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) if received < 2 => {
                return Err(failure(
                    "compiler.output",
                    "Compiler output reader disconnected",
                ));
            }
            _ => {}
        }
        if received < 2
            && !leaked_output
            && drain_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            terminate_tree(&mut child)?;
            leaked_output = true;
        }
    }
    // Cargo may have exited while an authored build-script descendant remains.
    // End the entire owned job/group even when the leader already exited.
    terminate_tree(&mut child)?;
    out_thread
        .join()
        .map_err(|_| failure("compiler.output", "stdout reader panicked"))?;
    err_thread
        .join()
        .map_err(|_| failure("compiler.output", "stderr reader panicked"))?;
    if leaked_output && !was_cancelled {
        return Err(failure(
            "compiler.output_leak",
            "Compiler descendant retained output pipes after Cargo exited",
        ));
    }
    Ok(CompilerOutput {
        status: status.expect("compiler leader was reaped"),
        stdout,
        stderr,
        cancelled: was_cancelled,
    })
}

fn read_bounded(mut reader: impl Read) -> std::io::Result<String> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if result.len() < OUTPUT_LIMIT {
            result.extend_from_slice(&buffer[..count.min(OUTPUT_LIMIT - result.len())]);
        }
    }
    Ok(String::from_utf8_lossy(&result).into_owned())
}

/// Owns and reaps a child process. Dropping it cancels a still-running child.
pub struct RunningGeneration {
    pub artifact: BuildArtifact,
    child: Box<dyn ChildWrapper>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    ready_file: PathBuf,
    stop_file: PathBuf,
    started: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationExit {
    pub success: bool,
    pub code: Option<i32>,
    pub cancelled: bool,
    pub forced: bool,
}

impl RunningGeneration {
    pub fn spawn(artifact: BuildArtifact, arguments: &[OsString]) -> Result<Self, BuildFailure> {
        let mut running = Self::prepare(artifact, arguments, &AtomicBool::new(false))?;
        running.start()?;
        Ok(running)
    }

    /// Start the executable behind its readiness barrier. No gameplay runs until
    /// `start` is called, so preparation cannot claim save/window ownership.
    pub fn prepare(
        artifact: BuildArtifact,
        arguments: &[OsString],
        cancel: &AtomicBool,
    ) -> Result<Self, BuildFailure> {
        Self::prepare_with_output(artifact, arguments, cancel, false)
    }

    /// As [`Self::prepare`]; `capture_stdout` pipes the generation's standard
    /// output for [`Self::take_stdout`] instead of inheriting the caller's.
    pub fn prepare_with_output(
        artifact: BuildArtifact,
        arguments: &[OsString],
        cancel: &AtomicBool,
        capture_stdout: bool,
    ) -> Result<Self, BuildFailure> {
        let control = format!(
            "control-{}",
            GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let ready_file = artifact.directory.join(format!("{control}.ready"));
        let stop_file = artifact.directory.join(format!("{control}.stop"));
        let mut command = Command::new(&artifact.executable);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .env("ENGINE_GENERATION_READY_FILE", &ready_file)
            .env("ENGINE_GENERATION_STOP_FILE", &stop_file);
        if capture_stdout {
            command.stdout(Stdio::piped());
        }
        let mut child = spawn_owned(command)?;
        let stdin = child.stdin().take();
        let stdout = child.stdout().take();
        let mut result = Self {
            artifact,
            child,
            stdin,
            stdout,
            ready_file,
            stop_file,
            started: false,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if let Some(exit) = result.try_wait()? {
                return Err(failure(
                    "process.ready",
                    format!("Generation exited before readiness: {:?}", exit.code),
                ));
            }
            if result.ready_file.is_file() {
                return Ok(result);
            }
            if std::time::Instant::now() >= deadline {
                return Err(failure(
                    "process.ready_timeout",
                    "Generation did not reach its startup barrier",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn start(&mut self) -> Result<(), BuildFailure> {
        if self.started {
            return Err(failure("process.started", "Generation has already started"));
        }
        let mut stdin = self
            .stdin
            .take()
            .ok_or_else(|| failure("process.start", "Generation control pipe is closed"))?;
        stdin
            .write_all(b"start\n")
            .and_then(|()| stdin.flush())
            .map_err(io_error)?;
        self.started = true;
        Ok(())
    }
    /// The piped standard output when prepared with `capture_stdout`.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> Result<Option<GenerationExit>, BuildFailure> {
        self.child.try_wait().map_err(io_error).map(|status| {
            status.map(|s| GenerationExit {
                success: s.success(),
                code: s.code(),
                cancelled: false,
                forced: false,
            })
        })
    }
    pub fn stop(&mut self) -> Result<GenerationExit, BuildFailure> {
        self.stop_with_timeout(Duration::from_secs(2))
    }
    pub fn stop_with_timeout(&mut self, timeout: Duration) -> Result<GenerationExit, BuildFailure> {
        if let Some(exit) = self.try_wait()? {
            // End descendants that outlived the leader; its status is known.
            terminate_tree(&mut self.child)?;
            return Ok(exit);
        }
        if self.started {
            fs::write(&self.stop_file, b"stop").map_err(io_error)?;
            let deadline = std::time::Instant::now() + timeout;
            while std::time::Instant::now() < deadline {
                if let Some(mut exit) = self.try_wait()? {
                    exit.cancelled = true;
                    return Ok(exit);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        terminate_tree(&mut self.child)?;
        let status = reap(&mut self.child)?;
        Ok(GenerationExit {
            success: false,
            code: status.code(),
            cancelled: true,
            forced: true,
        })
    }
    pub fn wait(&mut self, cancel: &AtomicBool) -> Result<GenerationExit, BuildFailure> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return self.stop();
            }
            if let Some(exit) = self.try_wait()? {
                return Ok(exit);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for RunningGeneration {
    fn drop(&mut self) {
        if self.stop().is_err() {
            let _ = terminate_tree(&mut self.child);
            let _ = reap(&mut self.child);
        }
        let _ = fs::remove_file(&self.ready_file);
        let _ = fs::remove_file(&self.stop_file);
    }
}

fn spawn_owned(command: Command) -> Result<Box<dyn ChildWrapper>, BuildFailure> {
    let mut command = CommandWrap::from(command);
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    command.spawn().map_err(io_error)
}

/// Waits for the leader after termination by polling. process_wrap's Windows
/// `JobObject::wait` blocks on the job completion port without a timeout, while
/// `try_wait` drains that port without caching the status, so calling `wait`
/// after any `try_wait` can block forever once the job's messages are consumed.
fn reap(child: &mut Box<dyn ChildWrapper>) -> Result<ExitStatus, BuildFailure> {
    loop {
        if let Some(status) = child.try_wait().map_err(io_error)? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn terminate_tree(child: &mut Box<dyn ChildWrapper>) -> Result<(), BuildFailure> {
    if let Err(error) = child.start_kill() {
        #[cfg(unix)]
        if error.raw_os_error() == Some(3) && child.try_wait().map_err(io_error)?.is_some() {
            return Ok(());
        }
        return Err(io_error(error));
    }
    Ok(())
}

/// Candidate compilation/readiness happen before the previous process is stopped.
/// The candidate's gameplay starts only after successful graceful old teardown.
pub struct GenerationManager {
    pub compiler: NativeCompiler,
    pub active: Option<RunningGeneration>,
}
impl GenerationManager {
    pub fn new(compiler: NativeCompiler) -> Self {
        Self {
            compiler,
            active: None,
        }
    }
    pub fn rebuild_and_replace(
        &mut self,
        request: &CompileRequest,
        arguments: &[OsString],
        cancel: &AtomicBool,
    ) -> Result<&mut RunningGeneration, BuildFailure> {
        let previous_good = self.compiler.last_good.clone();
        let candidate = self.compiler.compile(request, cancel)?;
        if cancel.load(Ordering::Acquire) {
            self.compiler.last_good = previous_good;
            return Err(cancelled());
        }
        let mut running = match RunningGeneration::prepare(candidate, arguments, cancel) {
            Ok(running) => running,
            Err(error) => {
                self.compiler.last_good = previous_good;
                return Err(error);
            }
        };
        if cancel.load(Ordering::Acquire) {
            self.compiler.last_good = previous_good;
            return Err(cancelled());
        }
        if let Some(old) = self.active.as_mut() {
            match old.stop() {
                Ok(exit) if exit.success && !exit.forced => {}
                Ok(_) => {
                    self.compiler.last_good = previous_good;
                    return Err(failure(
                        "process.teardown",
                        "Previous generation did not shut down cleanly; candidate not activated",
                    ));
                }
                Err(error) => {
                    self.compiler.last_good = previous_good;
                    return Err(error);
                }
            }
        }
        if let Err(error) = running.start() {
            self.compiler.last_good = previous_good;
            return Err(error);
        }
        self.active = Some(running);
        Ok(self.active.as_mut().expect("installed generation"))
    }
}

#[cfg(test)]
mod lifecycle_regressions {
    use super::*;

    #[test]
    fn cancellation_remains_live_after_leader_exits_with_inherited_pipes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("probe.rs");
        let executable = temp
            .path()
            .join(if cfg!(windows) { "probe.exe" } else { "probe" });
        fs::write(
            &source,
            r#"fn main(){
            if std::env::args().any(|a|a=="--child"){
                std::thread::sleep(std::time::Duration::from_secs(60));
            }else{
                std::process::Command::new(std::env::current_exe().unwrap()).arg("--child")
                    .stdin(std::process::Stdio::null()).spawn().unwrap();
            }
        }"#,
        )
        .unwrap();
        let cargo = Path::new(env!("CARGO"));
        let rustc = cargo
            .parent()
            .unwrap()
            .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
        assert!(
            Command::new(rustc)
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let output = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                cancel.store(true, Ordering::Release);
            });
            run_compiler(Command::new(executable), &cancel).unwrap()
        });
        assert!(output.cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[cfg(test)]
mod configuration_inputs_tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn package_without_inheritance_ignores_ancestor_workspace() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
        let game = temp.path().join("game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion='0.1.0'\n",
        );
        assert_eq!(inherited_workspace(&game).unwrap(), None);
    }

    #[test]
    fn inheriting_package_uses_nearest_ancestor_workspace() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
        write(
            &temp.path().join("outer/Cargo.toml"),
            "[workspace]\nmembers=['game']\n[workspace.package]\nversion='1.0.0'\n",
        );
        let game = temp.path().join("outer/game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion.workspace=true\n[dependencies]\nserde={workspace=true}\n",
        );
        assert_eq!(
            inherited_workspace(&game).unwrap(),
            Some(temp.path().join("outer/Cargo.toml").canonicalize().unwrap())
        );
    }

    #[test]
    fn explicit_workspace_key_and_own_root_are_respected() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("elsewhere/Cargo.toml"), "[workspace]\n");
        let explicit = temp.path().join("explicit");
        write(
            &explicit.join("Cargo.toml"),
            "[package]\nname='a'\nversion='0.1.0'\nworkspace='../elsewhere'\n",
        );
        assert_eq!(
            inherited_workspace(&explicit).unwrap(),
            Some(
                temp.path()
                    .join("elsewhere/Cargo.toml")
                    .canonicalize()
                    .unwrap()
            )
        );
        let own = temp.path().join("elsewhere/own");
        write(
            &own.join("Cargo.toml"),
            "[package]\nname='b'\nversion.workspace=true\n[workspace]\n[workspace.package]\nversion='1.0.0'\n",
        );
        assert_eq!(inherited_workspace(&own).unwrap(), None);
    }

    #[test]
    fn inheritance_without_any_workspace_is_a_diagnosed_failure() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        write(
            &game.join("Cargo.toml"),
            "[package]\nname='game'\nversion='0.1.0'\n[lints]\nworkspace=true\n",
        );
        // The temp directory's ancestors must not declare a workspace for this
        // negative case; skip rather than misreport on unusual hosts.
        if temp
            .path()
            .ancestors()
            .skip(1)
            .any(|a| a.join("Cargo.toml").is_file())
        {
            return;
        }
        assert_eq!(
            inherited_workspace(&game).unwrap_err().code,
            "compiler.workspace"
        );
    }

    #[test]
    fn output_location_variables_do_not_affect_identity() {
        let environment = fingerprint_environment();
        assert!(!environment.contains_key("CARGO_TARGET_DIR"));
        assert!(!environment.contains_key("CARGO_TARGET_TMPDIR"));
        assert!(!environment.contains_key("CARGO_MANIFEST_DIR"));
    }
}
