//! `engine scene inspect`.

use super::Outcome;
use super::errors::usage;
use super::options::options;
use engine_core::Result;
use engine_project::Project;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

pub(super) fn scene_inspect(arguments: &[OsString]) -> Result<Outcome> {
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
