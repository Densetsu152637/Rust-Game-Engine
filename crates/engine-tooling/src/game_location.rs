//! Where a packaged runtime finds its game when started without a project
//! argument: `game/project.json` beside the executable, or inside a macOS `.app`
//! bundle (`<X>.app/Contents/MacOS/<exe>`) `../Resources/game/project.json`.
use engine_project::{Error, Result};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

/// When set to `1`, the runtime prints the resolved `project.json` path and exits
/// without creating a window, GPU device or Steam session.
pub const RESOLVE_ONLY_ENV: &str = "ENGINE_RESOLVE_ONLY";

/// Candidate manifests for an executable in `exe_dir`, in lookup order.
pub fn bundled_project_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![exe_dir.join("game").join("project.json")];
    if exe_dir.file_name().is_some_and(|n| n == "MacOS")
        && let Some(contents) = exe_dir
            .parent()
            .filter(|c| c.file_name().is_some_and(|n| n == "Contents"))
    {
        candidates.push(contents.join("Resources").join("game").join("project.json"));
    }
    candidates
}

/// First candidate for which `is_file` is true.
pub fn resolve_bundled_project(
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<PathBuf> {
    let candidates = exe_dir.map(bundled_project_candidates).unwrap_or_default();
    candidates
        .iter()
        .find(|candidate| is_file(candidate))
        .cloned()
        .ok_or_else(|| {
            let searched = if candidates.is_empty() {
                "<executable directory unknown>".to_owned()
            } else {
                candidates
                    .iter()
                    .map(|c| c.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Error::new(
                "command.arguments",
                format!(
                    "Expected project manifest; none was given and no bundled game was found (searched: {searched})"
                ),
            )
        })
}

/// True when no project argument was given: no arguments, or the first one is an
/// option. Runs that pass a manifest first are unchanged.
pub fn lacks_project_argument(arguments: &[OsString]) -> bool {
    arguments
        .first()
        .is_none_or(|first| first.to_str().is_some_and(|s| s.starts_with("--")))
}

/// Arguments with the bundled manifest prepended when no project was given.
pub fn with_bundled_project(
    arguments: Vec<OsString>,
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<Vec<OsString>> {
    if !lacks_project_argument(&arguments) {
        return Ok(arguments);
    }
    let manifest = resolve_bundled_project(exe_dir, is_file)?;
    let mut result = Vec::with_capacity(arguments.len() + 1);
    result.push(manifest.into_os_string());
    result.extend(arguments);
    Ok(result)
}

/// Directory of the running executable.
pub fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// True when `ENGINE_RESOLVE_ONLY` is exactly `1`.
pub fn resolve_only_requested() -> bool {
    std::env::var_os(RESOLVE_ONLY_ENV).is_some_and(|v| v == "1")
}

/// Resolve-only mode: writes the absolute manifest path and a newline to `out`
/// and returns 0, or writes the message to `err` and returns 3.
pub fn resolve_only(
    arguments: Vec<OsString>,
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let resolved = with_bundled_project(arguments, exe_dir, &is_file).and_then(|args| {
        let manifest = PathBuf::from(&args[0]);
        if !is_file(&manifest) {
            return Err(Error::new(
                "command.arguments",
                format!("Project manifest not found: {}", manifest.display()),
            ));
        }
        std::path::absolute(&manifest)
            .map_err(|e| Error::new("command.arguments", format!("{}: {e}", manifest.display())))
    });
    match resolved {
        Ok(path) => match writeln!(out, "{}", path.display()) {
            Ok(()) => 0,
            Err(_) => 3,
        },
        Err(error) => {
            let message = error
                .diagnostics
                .first()
                .map_or("project resolution failed", |d| d.message.as_str());
            let _ = writeln!(err, "{message}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn project_in(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("project.json"), b"{}").unwrap();
    }

    fn run_resolve(arguments: &[&str], exe_dir: &Path) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = resolve_only(
            args(arguments),
            Some(exe_dir),
            Path::is_file,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn resolves_game_beside_the_executable() {
        let root = tempfile::tempdir().unwrap();
        project_in(&root.path().join("game"));
        let (code, out, _) = run_resolve(&[], root.path());
        assert_eq!(code, 0);
        let expected = root.path().join("game").join("project.json");
        assert_eq!(PathBuf::from(out.trim_end()), expected);
        assert!(Path::new(out.trim_end()).is_absolute());
    }

    #[test]
    fn resolves_game_inside_an_app_bundle() {
        let root = tempfile::tempdir().unwrap();
        let macos = root.path().join("Game.app").join("Contents").join("MacOS");
        fs::create_dir_all(&macos).unwrap();
        let resources_game = root
            .path()
            .join("Game.app")
            .join("Contents")
            .join("Resources")
            .join("game");
        project_in(&resources_game);
        let (code, out, _) = run_resolve(&[], &macos);
        assert_eq!(code, 0);
        assert_eq!(
            PathBuf::from(out.trim_end()),
            resources_game.join("project.json")
        );
    }

    #[test]
    fn game_beside_executable_wins_over_bundle_resources() {
        let root = tempfile::tempdir().unwrap();
        let contents = root.path().join("Game.app").join("Contents");
        let macos = contents.join("MacOS");
        project_in(&macos.join("game"));
        project_in(&contents.join("Resources").join("game"));
        let found = resolve_bundled_project(Some(&macos), Path::is_file).unwrap();
        assert_eq!(found, macos.join("game").join("project.json"));
    }

    #[test]
    fn fails_when_neither_layout_exists() {
        let root = tempfile::tempdir().unwrap();
        let (code, out, err) = run_resolve(&[], root.path());
        assert_ne!(code, 0);
        assert!(out.is_empty());
        assert!(err.contains("no bundled game was found"), "{err}");
        assert!(err.contains("project.json"), "{err}");
    }

    #[test]
    fn resources_layout_needs_app_bundle_shape() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("bin");
        fs::create_dir_all(&exe).unwrap();
        project_in(&root.path().join("Resources").join("game"));
        assert!(resolve_bundled_project(Some(&exe), Path::is_file).is_err());
    }

    #[test]
    fn explicit_project_argument_is_kept() {
        let root = tempfile::tempdir().unwrap();
        project_in(&root.path().join("game"));
        let explicit = root.path().join("other.json");
        fs::write(&explicit, b"{}").unwrap();
        let given = vec![explicit.clone().into_os_string(), "--headless".into()];
        let result = with_bundled_project(given.clone(), Some(root.path()), Path::is_file);
        assert_eq!(result.unwrap(), given);
        let (code, out, _) = run_resolve(&[explicit.to_str().unwrap()], root.path());
        assert_eq!(code, 0);
        assert_eq!(PathBuf::from(out.trim_end()), explicit);
    }

    #[test]
    fn leading_option_uses_the_bundled_game_and_keeps_options() {
        let root = tempfile::tempdir().unwrap();
        project_in(&root.path().join("game"));
        let result = with_bundled_project(
            args(&["--headless", "--ticks", "3"]),
            Some(root.path()),
            Path::is_file,
        )
        .unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(
            PathBuf::from(&result[0]),
            root.path().join("game").join("project.json")
        );
        assert_eq!(result[1], "--headless");
    }

    #[test]
    fn missing_explicit_project_fails_in_resolve_only_mode() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("nope.json");
        let (code, _, err) = run_resolve(&[missing.to_str().unwrap()], root.path());
        assert_ne!(code, 0);
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn unknown_executable_directory_is_reported() {
        let error = resolve_bundled_project(None, |_| true).unwrap_err();
        assert!(error.diagnostics[0].message.contains("unknown"));
    }
}
