//! Project-level tools: `project_info`, `project_validate`, `input_actions`.

use super::{Spec, clean, slash, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_project::Project;
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::Path};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    vec![
        tool(
            Spec {
                name: "project_info",
                title: "Project overview",
                description: "Summarise the opened engine project: name, schema, startup scene, \
                    every scene, asset counts by kind, script assets, the native Rust crate, input \
                    actions and the access level this server was started with. Call this first to \
                    orient yourself; follow up with scene_list, asset_list or input_actions for \
                    detail.",
                input: schema::object(vec![], &[]),
                output: schema::output(vec![
                    ("name", schema::string("Project name.")),
                    ("schema", json!({"type": "integer"})),
                    (
                        "manifest",
                        schema::string("Project-relative manifest path."),
                    ),
                    (
                        "startup_scene",
                        schema::string("Project-relative startup scene path."),
                    ),
                    (
                        "scenes",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("assets", json!({"type": "object"})),
                    (
                        "scripts",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("rust", json!({"type": "object"})),
                    (
                        "input_actions",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("settings", json!({"type": "object"})),
                    ("access", json!({"type": "object"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            project_info,
        ),
        tool(
            Spec {
                name: "project_validate",
                title: "Validate project",
                description: "Statically validate a project manifest and every authored file it \
                    references (scenes, meshes, textures, fonts, scripts) without compiling or \
                    running native game code. Returns ok:true, or ok:false with diagnostics \
                    (code, severity, file, path, message). Compiler errors are not covered: use \
                    project_build for those.",
                input: schema::object(
                    vec![(
                        "manifest",
                        json!({"type": "string", "default": "project.json",
                            "description": "Project-relative manifest path."}),
                    )],
                    &[],
                ),
                output: schema::output(vec![
                    ("command", schema::string("Always `project validate`.")),
                    ("project", schema::string("Project name.")),
                    ("scenes", json!({"type": "integer"})),
                    ("assets", json!({"type": "integer"})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            project_validate,
        ),
        tool(
            Spec {
                name: "input_actions",
                title: "Input actions",
                description: "List the project's input actions with their bindings. Action names \
                    are what scripts query and what project_run_headless and render_screenshot \
                    accept as scripted input. Optionally include the full list of valid \
                    binding tokens (for example `Key:Right`, `Gamepad:South`).",
                input: schema::object(
                    vec![(
                        "include_binding_tokens",
                        schema::boolean("Also return every valid binding token.", false),
                    )],
                    &[],
                ),
                output: schema::output(vec![
                    (
                        "actions",
                        json!({"type": "array", "items": {"type": "object",
                        "properties": {"name": {"type": "string"},
                            "bindings": {"type": "array", "items": {"type": "string"}},
                            "legacy_key": {"type": "string"}}}}),
                    ),
                    (
                        "binding_tokens",
                        json!({"type": "array", "items": {"type": "string"}}),
                    ),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            input_actions,
        ),
    ]
}

fn project_info(cx: &ToolContext<'_>, _args: &Map<String, Value>) -> ToolOutcome {
    let project = cx.project()?;
    let manifest = paths::display_relative(cx.root, &project.manifest);
    let startup = slash(&project.startup_scene);

    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut scripts = Vec::new();
    let mut scenes = vec![json!({"path": startup, "startup": true, "scene_id": project.scene.id})];
    for asset in project.assets.values() {
        *by_kind.entry(asset.kind.as_str()).or_default() += 1;
        match asset.kind.as_str() {
            "script" => scripts.push(json!({"asset_id": asset.id, "path": slash(&asset.path)})),
            "scene" => {
                let path = slash(&asset.path);
                if path == startup {
                    scenes[0]["asset_id"] = json!(asset.id);
                } else {
                    scenes.push(json!({"path": path, "startup": false, "asset_id": asset.id}));
                }
            }
            _ => {}
        }
    }

    let crate_manifest = project.rust_manifest().ok();
    let rust = json!({
        "manifest": slash(&project.rust_manifest),
        "present": crate_manifest.is_some(),
        "package_name": crate_manifest.as_deref().and_then(package_name),
        "script_assets": scripts.len(),
    });
    let camera = project
        .scene
        .entities
        .iter()
        .find_map(|e| e.components.camera_2d.as_ref())
        .map(|c| json!({"logical_size": c.logical_size, "pixels_per_unit": c.pixels_per_unit}));

    Ok(ToolResult::new(json!({
        "name": project.name,
        "schema": project.schema,
        "manifest": manifest,
        "startup_scene": startup,
        "scenes": scenes,
        "assets": {"total": project.assets.len(), "by_kind": by_kind},
        "scripts": scripts,
        "rust": rust,
        "input_actions": action_list(&project),
        "settings": {"startup_camera": camera, "fixed_delta_seconds": 1.0 / 60.0},
        "access": {"read": true, "write": cx.access.write, "exec": cx.access.exec},
    })))
}

/// `[package] name` of a Cargo manifest, read with a tiny line scanner.
fn package_name(manifest: &Path) -> Option<String> {
    let bytes = paths::read_bounded(manifest, 256 * 1024).ok()?;
    let text = paths::as_text(&bytes)?;
    let mut in_package = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package
            && let Some(value) = line.strip_prefix("name")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn action_list(project: &Project) -> Vec<Value> {
    project
        .input_bindings
        .iter()
        .map(|(name, bindings)| {
            let mut action = json!({"name": name, "bindings": bindings});
            if let Some(key) = project.input_actions.get(name) {
                action["legacy_key"] = json!(key);
            }
            action
        })
        .collect()
}

fn project_validate(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let manifest = args
        .get("manifest")
        .cloned()
        .unwrap_or_else(|| json!("project.json"));
    let file = cx.existing_file("manifest", &manifest)?;
    let project = Project::load(file).map_err(ToolFailure::from)?;
    let scenes = 1 + project
        .assets
        .values()
        .filter(|a| a.kind == "scene" && slash(&a.path) != slash(&project.startup_scene))
        .count();
    Ok(ToolResult::new(json!({
        "command": "project validate",
        "diagnostics": [],
        "project": project.name,
        "scenes": scenes,
        "assets": project.assets.len(),
    })))
}

fn input_actions(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let include = Args(args).boolean("include_binding_tokens", false)?;
    let project = cx.project()?;
    let mut result = json!({"actions": action_list(&project)});
    if include {
        result["binding_tokens"] = clean(&engine_project::input::binding_registry());
    }
    Ok(ToolResult::new(result))
}
