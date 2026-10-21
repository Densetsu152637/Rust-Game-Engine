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
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static GENERATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Every field affects generation identity. `main_source` is the host's real
/// native entry point and may invoke unrestricted public engine/game APIs.
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub fingerprint: String,
    pub generation: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
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
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let engine_root = request.engine_root.canonicalize().map_err(io_error)?;
        let manifest = request.project_manifest.canonicalize().map_err(io_error)?;
        let project_root = manifest
            .parent()
            .ok_or_else(|| failure("compiler.manifest", "Manifest has no parent"))?;
        let package_name = package_name(&manifest)?;
        let compiler_identity = self.identity()?;
        fs::create_dir_all(&request.cache_root).map_err(io_error)?;
        let cache = request.cache_root.canonicalize().map_err(io_error)?;
        if cache == engine_root || cache == project_root {
            return Err(failure(
                "compiler.cache",
                "Cache must be a separate generated directory",
            ));
        }
        let generation = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            GENERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let directory = cache.join(&generation);
        fs::create_dir(&directory).map_err(io_error)?;
        fs::create_dir(directory.join("src")).map_err(io_error)?;
        let binary_name = format!("runtime-{generation}");
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
        let mut resolution = self.cargo_command(&directory, request);
        resolution.arg("metadata").arg("--format-version=1");
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
        let metadata: serde_json::Value =
            serde_json::from_str(&resolved.stdout).map_err(|e| failure("compiler.metadata", e))?;
        let roots = local_package_roots(&metadata, &directory)?;
        let before = fingerprint(
            request,
            &engine_root,
            &roots,
            &directory,
            &compiler_identity,
        )?;
        let mut command = self.cargo_command(&directory, request);
        command
            .arg("build")
            .arg("--manifest-path")
            .arg(directory.join("Cargo.toml"))
            .arg("--message-format=json")
            .arg("--locked")
            .arg("--target-dir")
            .arg(cache.join("target"));
        if request.offline {
            command.arg("--offline");
        }
        if request.release {
            command.arg("--release");
        }
        if let Some(target) = &request.target {
            command.arg("--target").arg(target);
        }
        let output = run_compiler(command, cancel)?;
        let mut diagnostics = Vec::new();
        let mut executable = None;
        for line in output.stdout.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                match value["reason"].as_str() {
                    Some("compiler-message") => diagnostics.push(value["message"].clone()),
                    Some("compiler-artifact") if value["target"]["name"] == binary_name => {
                        executable = value["executable"].as_str().map(PathBuf::from);
                    }
                    _ => {}
                }
            }
        }
        if !output.status.success() || output.cancelled {
            return Err(BuildFailure {
                code: if output.cancelled {
                    "compiler.cancelled"
                } else {
                    "compiler.build"
                }
                .into(),
                message: if output.cancelled {
                    "Compilation cancelled"
                } else {
                    "Native Rust compilation failed"
                }
                .into(),
                diagnostics,
                stderr: output.stderr,
                exit_code: output.status.code(),
                cancelled: output.cancelled,
            });
        }
        if fingerprint(
            request,
            &engine_root,
            &roots,
            &directory,
            &compiler_identity,
        )? != before
        {
            return Err(failure(
                "compiler.source_changed",
                "Sources changed during compilation; previous generation retained",
            ));
        }
        let built = executable.ok_or_else(|| {
            failure(
                "compiler.artifact",
                "Cargo succeeded without a runtime executable artifact",
            )
        })?;
        // Cargo uses a shared target cache; copy the binary out before another
        // build can replace it or Windows locks a running generation's image.
        let executable = directory.join(if cfg!(windows) {
            "runtime.exe"
        } else {
            "runtime"
        });
        fs::copy(built, &executable).map_err(io_error)?;
        let artifact = BuildArtifact {
            fingerprint: before,
            generation,
            executable,
            directory,
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

    fn identity(&self) -> Result<String, BuildFailure> {
        let mut identity = String::new();
        for (program, argument) in [(&self.rustc, "-vV"), (&self.cargo, "--version")] {
            let output = Command::new(program)
                .arg(argument)
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

    fn cargo_command(&self, directory: &Path, request: &CompileRequest) -> Command {
        let mut command = Command::new(&self.cargo);
        command.env("RUSTC", &self.rustc).current_dir(directory);
        if let Some(target) = &request.target {
            command.env("CARGO_BUILD_TARGET", target);
        }
        command
    }
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

fn fingerprint(
    request: &CompileRequest,
    engine: &Path,
    roots: &[PathBuf],
    generation: &Path,
    identity: &str,
) -> Result<String, BuildFailure> {
    let mut hash = Sha256::new();
    fn field(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    field(&mut hash, b"engine-native-generation-v1");
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
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_TARGET",
        "CC",
        "CXX",
    ] {
        field(&mut hash, name.as_bytes());
        field(
            &mut hash,
            std::env::var_os(name)
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
    }
    for root in roots {
        field(&mut hash, root.to_string_lossy().as_bytes());
        let mut files = BTreeMap::new();
        let cache = request.cache_root.canonicalize().map_err(io_error)?;
        collect_sources(root, root, &cache, &mut files)?;
        for (relative, path) in files {
            field(&mut hash, relative.as_bytes());
            field(&mut hash, &fs::read(path).map_err(io_error)?);
        }
    }
    // Workspace/compiler configuration lives above individual package roots.
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain",
        "rust-toolchain.toml",
        ".cargo/config",
        ".cargo/config.toml",
    ] {
        let path = engine.join(name);
        if path.is_file() {
            field(&mut hash, name.as_bytes());
            field(&mut hash, &fs::read(path).map_err(io_error)?);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
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
        if entry.file_type().map_err(io_error)?.is_symlink() {
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
    let was_cancelled;
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            terminate_tree(&mut child)?;
            was_cancelled = true;
            break child.wait().map_err(io_error)?;
        }
        if let Some(status) = child.try_wait().map_err(io_error)? {
            was_cancelled = false;
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    for (is_stdout, output) in receive.iter().take(2) {
        let output = output.map_err(io_error)?;
        if is_stdout {
            stdout = output;
        } else {
            stderr = output;
        }
    }
    out_thread
        .join()
        .map_err(|_| failure("compiler.output", "stdout reader panicked"))?;
    err_thread
        .join()
        .map_err(|_| failure("compiler.output", "stderr reader panicked"))?;
    Ok(CompilerOutput {
        status,
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
        let mut child = spawn_owned(command)?;
        let stdin = child.stdin().take();
        let mut result = Self {
            artifact,
            child,
            stdin,
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
        let status = self.child.wait().map_err(io_error)?;
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
            let _ = self.child.wait();
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

fn terminate_tree(child: &mut Box<dyn ChildWrapper>) -> Result<(), BuildFailure> {
    if child.try_wait().map_err(io_error)?.is_some() {
        return Ok(());
    }
    child.start_kill().map_err(io_error)?;
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
