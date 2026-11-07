//! Locating the FFmpeg and ffprobe executables. FFmpeg is never bundled by the
//! engine; a game may ship its own copy in a `tools` folder next to the runtime.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable that names the ffmpeg executable explicitly.
pub const FFMPEG_ENV: &str = "ENGINE_VIDEO_FFMPEG";
/// Environment variable that names the ffprobe executable explicitly.
pub const FFPROBE_ENV: &str = "ENGINE_VIDEO_FFPROBE";

/// Directories searched for a bundled tool, in order, for a runtime executable in
/// `exe_dir`: `<exe_dir>/tools`, then `Contents/Resources/tools` when `exe_dir` is
/// `<X>.app/Contents/MacOS`.
pub fn bundled_tool_dirs(exe_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![exe_dir.join("tools")];
    let contents = exe_dir.parent();
    if exe_dir.file_name().is_some_and(|n| n == "MacOS")
        && let Some(contents) = contents.filter(|c| c.file_name().is_some_and(|n| n == "Contents"))
    {
        dirs.push(contents.join("Resources").join("tools"));
    }
    dirs
}

/// Pure lookup for one tool. Order: the non-empty `env_value`, then
/// `<tools dir>/<file>` for each bundled directory where `exists` is true, then the
/// bare `name` (resolved through `PATH` when spawned). `windows` appends `.exe` to
/// the bundled file name.
pub fn resolve_tool(
    name: &str,
    env_value: Option<OsString>,
    exe_dir: Option<&Path>,
    windows: bool,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(value) = env_value.filter(|v| !v.is_empty()) {
        return value.into();
    }
    let file = if windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    exe_dir
        .into_iter()
        .flat_map(bundled_tool_dirs)
        .map(|dir| dir.join(&file))
        .find(|candidate| exists(candidate))
        .unwrap_or_else(|| name.into())
}

/// Resolves `name` for the running process: `env_var`, then bundled folders next to
/// the current executable, then `PATH`.
pub fn discover_tool(name: &str, env_var: &str) -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    resolve_tool(
        name,
        std::env::var_os(env_var),
        exe_dir.as_deref(),
        cfg!(windows),
        Path::is_file,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |candidate| paths.iter().any(|p| p == candidate)
    }

    #[test]
    fn env_wins_over_bundled_and_path() {
        let found = resolve_tool(
            "ffmpeg",
            Some("/custom/ffmpeg".into()),
            Some(Path::new("/app")),
            false,
            present(&["/app/tools/ffmpeg"]),
        );
        assert_eq!(found, PathBuf::from("/custom/ffmpeg"));
    }

    #[test]
    fn bundled_tools_win_over_path() {
        let found = resolve_tool(
            "ffmpeg",
            None,
            Some(Path::new("/app")),
            false,
            present(&["/app/tools/ffmpeg"]),
        );
        assert_eq!(found, Path::new("/app").join("tools").join("ffmpeg"));
    }

    #[test]
    fn windows_looks_for_exe_suffix() {
        let exe = Path::new("/app").join("tools").join("ffmpeg.exe");
        let found = resolve_tool("ffmpeg", None, Some(Path::new("/app")), true, |p| p == exe);
        assert_eq!(found, exe);
        let missing = resolve_tool("ffmpeg", None, Some(Path::new("/app")), true, |_| false);
        assert_eq!(missing, PathBuf::from("ffmpeg"));
    }

    #[test]
    fn falls_back_to_path_name() {
        let found = resolve_tool("ffprobe", None, Some(Path::new("/app")), false, |_| false);
        assert_eq!(found, PathBuf::from("ffprobe"));
        let no_dir = resolve_tool("ffprobe", None, None, false, |_| true);
        assert_eq!(no_dir, PathBuf::from("ffprobe"));
    }

    #[test]
    fn empty_env_is_ignored() {
        let found = resolve_tool("ffmpeg", Some(OsString::new()), None, false, |_| false);
        assert_eq!(found, PathBuf::from("ffmpeg"));
    }

    #[test]
    fn app_bundle_resources_are_searched_after_exe_tools() {
        let exe_dir = Path::new("/Applications/Game.app/Contents/MacOS");
        let resources = exe_dir
            .parent()
            .unwrap()
            .join("Resources")
            .join("tools")
            .join("ffmpeg");
        let found = resolve_tool("ffmpeg", None, Some(exe_dir), false, |p| p == resources);
        assert_eq!(found, resources);
        let beside = exe_dir.join("tools").join("ffmpeg");
        let both = resolve_tool("ffmpeg", None, Some(exe_dir), false, |p| {
            p == resources || p == beside
        });
        assert_eq!(both, beside);
    }

    #[test]
    fn resources_directory_requires_app_bundle_layout() {
        assert_eq!(
            bundled_tool_dirs(Path::new("/opt/game/MacOS")),
            vec![PathBuf::from("/opt/game/MacOS").join("tools")]
        );
    }

    #[test]
    fn discover_uses_real_files_next_to_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        let file = tools.join("ffmpeg");
        std::fs::write(&file, b"x").unwrap();
        let found = resolve_tool("ffmpeg", None, Some(dir.path()), false, Path::is_file);
        assert_eq!(found, file);
    }
}
