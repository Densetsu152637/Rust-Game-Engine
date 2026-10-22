//! Cargo and toolchain invocation for generated workspaces.

use super::native::{NativeCompiler, PreparedGeneration};
use super::process::{CompilerOutput, run_compiler};
use super::{BuildFailure, CompileRequest, failure, io_error};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

impl NativeCompiler {
    pub(super) fn run_cargo(
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

    /// Cargo discovers `.cargo/config*` from its working directory upwards. Using
    /// the selected toolchain's own directory keeps configuration above the game
    /// package, project or generated cache out of the build. Rustup proxies also
    /// resolve their toolchain from this directory rather than from the project.
    pub(super) fn neutral_directory(&self) -> Result<PathBuf, BuildFailure> {
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

    pub(super) fn identity(&self, neutral: &Path) -> Result<String, BuildFailure> {
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

    pub(super) fn cargo_command(&self, neutral: &Path, request: &CompileRequest) -> Command {
        let mut command = Command::new(&self.cargo);
        command.env("RUSTC", &self.rustc).current_dir(neutral);
        if let Some(target) = &request.target {
            command.env("CARGO_BUILD_TARGET", target);
        }
        command
    }
}

pub(super) fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .and_then(|path| path.canonicalize().ok())
}
