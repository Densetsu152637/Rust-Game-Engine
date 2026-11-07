//! Linux AppImage output: the staged package becomes an AppDir that
//! `appimagetool` turns into a single executable file.
//!
//! Everything up to the tool call is plain file generation ([`write_appdir_files`])
//! and runs on every OS. Only [`export_new_appimage`] needs a Linux host and the
//! tool; both are checked by [`AppImageTooling::resolve`] before anything is staged.
//!
//! AppDir layout: the staged package (`runtime`, `game/`, `licenses/`, ...) plus
//! `AppRun`, `<id>.desktop`, `<id>.png` and `.DirIcon` at the root.

use engine_project::{Error, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Environment variable naming the `appimagetool` executable.
pub const TOOL_ENV: &str = "ENGINE_APPIMAGETOOL";
/// Environment variable naming a pinned type2 runtime file for `--runtime-file`.
pub const RUNTIME_ENV: &str = "ENGINE_APPIMAGE_RUNTIME";
/// Name searched on `PATH` when [`TOOL_ENV`] is unset.
const TOOL_NAME: &str = "appimagetool";
/// Longest sanitised id; the id names the AppDir, the `.desktop` file and the icon.
const MAX_ID_LENGTH: usize = 64;
const ICON_SIZE: u32 = 256;
/// Id used when a project name has no ASCII letter or digit.
const FALLBACK_ID: &str = "game";

const APP_RUN: &str = "#!/bin/sh\n\
if [ -z \"$APPDIR\" ]; then\n\
\tAPPDIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd) || exit 3\n\
fi\n\
exec \"$APPDIR/runtime\" \"$@\"\n";

/// Where the host runs `appimagetool` from, validated.
#[derive(Clone, Debug)]
pub struct AppImageTooling {
    /// The `appimagetool` executable.
    pub tool: PathBuf,
    /// Pinned type2 runtime passed as `--runtime-file`, so the tool never downloads.
    pub runtime_file: Option<PathBuf>,
}

impl AppImageTooling {
    /// Resolve from the process: Linux host, then `ENGINE_APPIMAGETOOL` or `PATH`.
    pub fn from_process() -> Result<Self> {
        Self::resolve(
            cfg!(target_os = "linux"),
            std::env::var_os(TOOL_ENV).as_deref(),
            std::env::var_os("PATH").as_deref(),
            std::env::var_os(RUNTIME_ENV).as_deref(),
        )
    }

    /// The host and tool lookup with every input injected.
    ///
    /// - `tooling.appimage.unsupported_host`: not a Linux host.
    /// - `tooling.appimage.tool_missing`: no `appimagetool`; a non-empty
    ///   `tool_env` is the only candidate and `PATH` is then not searched.
    /// - `tooling.appimage.runtime_missing`: `runtime_env` is set but not a file.
    pub fn resolve(
        linux_host: bool,
        tool_env: Option<&OsStr>,
        path_env: Option<&OsStr>,
        runtime_env: Option<&OsStr>,
    ) -> Result<Self> {
        if !linux_host {
            return Err(Error::new(
                "tooling.appimage.unsupported_host",
                "--format appimage is built on a Linux host only",
            ));
        }
        let tool = match tool_env.filter(|value| !value.is_empty()) {
            Some(explicit) => Some(PathBuf::from(explicit)).filter(|path| path.is_file()),
            None => path_env.and_then(find_on_path),
        }
        .ok_or_else(|| {
            Error::new(
                "tooling.appimage.tool_missing",
                format!(
                    "appimagetool was not found; set {TOOL_ENV} to its path or put {TOOL_NAME} on PATH"
                ),
            )
        })?;
        let runtime_file = match runtime_env.filter(|value| !value.is_empty()) {
            Some(value) => {
                let path = PathBuf::from(value);
                if !path.is_file() {
                    return Err(Error::new(
                        "tooling.appimage.runtime_missing",
                        format!("{RUNTIME_ENV} does not name a file"),
                    ));
                }
                Some(path)
            }
            None => None,
        };
        Ok(Self { tool, runtime_file })
    }
}

fn find_on_path(path_env: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .map(|directory| directory.join(TOOL_NAME))
        .find(|candidate| candidate.is_file())
}

/// Lowercase ASCII id of a project name, used for the AppDir, `.desktop` and icon
/// file names and as the icon name. ASCII letters and digits are kept (letters
/// lowercased); every other run of characters, including non-ASCII ones, becomes
/// one `-`. Leading and trailing `-` are dropped and the id is cut to 64
/// characters. A name with no ASCII letter or digit gives `game`.
pub fn sanitize_id(name: &str) -> String {
    let mut id = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            id.push(character.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    id.truncate(MAX_ID_LENGTH);
    let id = id.trim_end_matches('-');
    if id.is_empty() {
        FALLBACK_ID.to_owned()
    } else {
        id.to_owned()
    }
}

/// The text of `<id>.desktop`.
pub fn desktop_entry(id: &str, display_name: &str) -> String {
    // Desktop Entry strings are one line; a backslash starts an escape.
    let single_line: String = display_name
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let name = single_line.trim().replace('\\', "\\\\");
    let name = if name.is_empty() { id } else { &name };
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec=runtime\nIcon={id}\nCategories=Game;\nTerminal=false\n"
    )
}

/// The default engine icon: a 256x256 PNG, identical on every run.
pub fn default_icon() -> Result<Vec<u8>> {
    use image::{ImageFormat, Rgba, RgbaImage};
    let background = Rgba([24, 32, 56, 255]);
    let border = Rgba([232, 236, 244, 255]);
    let fill = Rgba([88, 140, 232, 255]);
    let image = RgbaImage::from_fn(ICON_SIZE, ICON_SIZE, |x, y| {
        let inside = |low: u32, high: u32| (low..high).contains(&x) && (low..high).contains(&y);
        if inside(72, 184) {
            if inside(80, 176) { fill } else { border }
        } else {
            background
        }
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|error| Error::new("package.appimage.icon", error.to_string()))?;
    Ok(bytes.into_inner())
}

/// Add the AppImage files to an AppDir that already holds the staged package.
/// Returns the sanitised id.
pub fn write_appdir_files(appdir: &Path, display_name: &str) -> Result<String> {
    let id = sanitize_id(display_name);
    fs::write(appdir.join("AppRun"), APP_RUN)?;
    fs::write(
        appdir.join(format!("{id}.desktop")),
        desktop_entry(&id, display_name),
    )?;
    let icon = default_icon()?;
    fs::write(appdir.join(format!("{id}.png")), &icon)?;
    fs::write(appdir.join(".DirIcon"), &icon)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for executable in [appdir.join("AppRun"), appdir.join("runtime")] {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(id)
}

/// Populate an AppDir with `populate` (the staged package), add the AppImage
/// files, run `appimagetool` and publish the result at `destination` only when
/// complete. An existing destination is refused before and again when publishing.
pub fn export_new_appimage(
    destination: &Path,
    display_name: &str,
    tooling: &AppImageTooling,
    populate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if destination.exists() {
        return Err(exists());
    }
    if destination.file_name().is_none() {
        return Err(Error::new(
            "tooling.destination.invalid",
            "Destination requires a file name",
        ));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let staging = tempfile::Builder::new()
        .prefix(".engine-export-")
        .tempdir_in(&parent)?;
    let appdir = staging
        .path()
        .join(format!("{}.AppDir", sanitize_id(display_name)));
    fs::create_dir(&appdir)?;
    populate(&appdir)?;
    write_appdir_files(&appdir, display_name)?;
    let output = tempfile::Builder::new()
        .prefix(".engine-export-")
        .suffix(".partial")
        .tempfile_in(&parent)?
        .into_temp_path();
    run_tool(tooling, &appdir, &output)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
    }
    output.persist_noclobber(destination).map_err(|error| {
        if destination.exists() {
            exists()
        } else {
            Error::from(error.error)
        }
    })?;
    Ok(())
}

fn run_tool(tooling: &AppImageTooling, appdir: &Path, output: &Path) -> Result<()> {
    let mut command = Command::new(&tooling.tool);
    // Build runners have no FUSE; the tool is itself an AppImage.
    command.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    if std::env::var_os("ARCH").is_none_or(|value| value.is_empty())
        && let Some(arch) = tool_arch(std::env::consts::ARCH)
    {
        command.env("ARCH", arch);
    }
    command.arg("--no-appstream");
    if let Some(runtime) = &tooling.runtime_file {
        command.arg("--runtime-file").arg(runtime);
    }
    command
        .arg(appdir)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = command.output().map_err(|error| {
        Error::new(
            "tooling.appimage.failed",
            format!("Could not run {}: {error}", tooling.tool.display()),
        )
    })?;
    let produced = fs::metadata(output).is_ok_and(|metadata| metadata.len() > 0);
    if !result.status.success() || !produced {
        let mut text = String::from_utf8_lossy(&result.stderr).into_owned();
        text.push_str(&String::from_utf8_lossy(&result.stdout));
        let tail = text.trim();
        let start = tail.len().saturating_sub(2000);
        let start = (start..=tail.len())
            .find(|&index| tail.is_char_boundary(index))
            .unwrap_or(tail.len());
        return Err(Error::new(
            "tooling.appimage.failed",
            format!(
                "appimagetool {}: {}",
                if result.status.success() {
                    "wrote no file".to_owned()
                } else {
                    format!("exited with {}", result.status)
                },
                &tail[start..]
            ),
        ));
    }
    Ok(())
}

/// The `ARCH` value `appimagetool` expects for a Rust architecture name.
fn tool_arch(rust_arch: &str) -> Option<&'static str> {
    match rust_arch {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        "x86" => Some("i686"),
        "arm" => Some("armhf"),
        _ => None,
    }
}

fn exists() -> Error {
    Error::new("tooling.destination.exists", "Destination must be new")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_lowercase_ascii_with_single_dashes() {
        let cases = [
            ("First Project", "first-project"),
            ("  My_Game!! 2  ", "my-game-2"),
            ("Café Ünïcode", "caf-n-code"),
            ("---", "game"),
            ("日本語", "game"),
            ("", "game"),
            ("A--B", "a-b"),
            ("x.y/z", "x-y-z"),
        ];
        for (name, id) in cases {
            assert_eq!(sanitize_id(name), id, "{name:?}");
        }
        let long = sanitize_id(&format!("{}-tail", "a".repeat(63)));
        assert_eq!(long, "a".repeat(63));
        assert_eq!(sanitize_id(&"b".repeat(100)).len(), MAX_ID_LENGTH);
    }

    #[test]
    fn desktop_entry_is_one_line_per_key_and_escapes_the_name() {
        let text = desktop_entry("first-project", "First\nProject \\ 1");
        assert_eq!(
            text,
            "[Desktop Entry]\nType=Application\nName=First Project \\\\ 1\nExec=runtime\nIcon=first-project\nCategories=Game;\nTerminal=false\n"
        );
        assert!(desktop_entry("game", "  \n ").contains("Name=game\n"));
    }

    #[test]
    fn default_icon_is_a_deterministic_256_square_png() {
        let first = default_icon().unwrap();
        assert_eq!(first, default_icon().unwrap());
        assert_eq!(&first[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&first).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
    }

    #[test]
    fn app_run_derives_appdir_and_forwards_arguments() {
        assert!(APP_RUN.starts_with("#!/bin/sh\n"));
        assert!(APP_RUN.contains("if [ -z \"$APPDIR\" ]"));
        assert!(APP_RUN.ends_with("exec \"$APPDIR/runtime\" \"$@\"\n"));
    }

    #[test]
    fn resolve_checks_host_then_tool_then_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let tool = temp.path().join("appimagetool");
        let runtime = temp.path().join("runtime-file");
        fs::write(&tool, "").unwrap();
        fs::write(&runtime, "").unwrap();
        let code =
            |result: Result<AppImageTooling>| result.unwrap_err().diagnostics[0].code.clone();

        let path = std::env::join_paths([temp.path()]).unwrap();
        assert_eq!(
            code(AppImageTooling::resolve(
                false,
                Some(tool.as_os_str()),
                None,
                None
            )),
            "tooling.appimage.unsupported_host"
        );
        assert_eq!(
            code(AppImageTooling::resolve(true, None, None, None)),
            "tooling.appimage.tool_missing"
        );
        // An explicit tool replaces the PATH search, even a bad one.
        let missing = temp.path().join("nope");
        assert_eq!(
            code(AppImageTooling::resolve(
                true,
                Some(missing.as_os_str()),
                Some(path.as_os_str()),
                None
            )),
            "tooling.appimage.tool_missing"
        );
        assert_eq!(
            code(AppImageTooling::resolve(
                true,
                Some(tool.as_os_str()),
                None,
                Some(missing.as_os_str())
            )),
            "tooling.appimage.runtime_missing"
        );
        let found = AppImageTooling::resolve(true, None, Some(path.as_os_str()), None).unwrap();
        assert_eq!(found.tool, tool);
        assert_eq!(found.runtime_file, None);
        // Empty variables count as unset.
        let found = AppImageTooling::resolve(
            true,
            Some(OsStr::new("")),
            Some(path.as_os_str()),
            Some(OsStr::new("")),
        )
        .unwrap();
        assert_eq!(found.tool, tool);
        let found = AppImageTooling::resolve(
            true,
            Some(tool.as_os_str()),
            None,
            Some(runtime.as_os_str()),
        )
        .unwrap();
        assert_eq!(found.runtime_file, Some(runtime));
    }

    #[test]
    fn tool_arch_maps_known_architectures() {
        assert_eq!(tool_arch("x86_64"), Some("x86_64"));
        assert_eq!(tool_arch("aarch64"), Some("aarch64"));
        assert_eq!(tool_arch("riscv64"), None);
    }
}
