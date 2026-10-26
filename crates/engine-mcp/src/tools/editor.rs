//! Persisted editor settings only; these tools do not control a running editor cache.
use super::{Spec, clean, navigation::publish_new_file, tool};
use crate::{
    config::AccessLevel,
    paths,
    registry::{Annotations, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use engine_editor_settings::{EditorSettings, MAX_DOCUMENT_BYTES, SETTINGS_FILE};
use serde_json::{Map, Value, json};

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let output = || {
        schema::output(vec![
            (
                "path",
                schema::string("Always editor.json in the project root."),
            ),
            ("exists", json!({"type":"boolean"})),
            (
                "revision",
                json!({"type":["string","null"],"description":"Exact persisted byte hash, or null for missing settings."}),
            ),
            (
                "settings",
                json!({"type":"object","description":"Validated settings, including defaults."}),
            ),
        ])
    };
    vec![
        tool(
            Spec {
                name: "editor_settings_get",
                title: "Read persisted editor settings",
                description: "Read validated project editor.json (snapping, grid, gizmo, camera bookmarks). Missing settings return defaults without writing. Returns a persisted byte revision for editor_settings_update. Does not inspect or refresh a running editor's cache.",
                input: schema::object(vec![], &[]),
                output: output(),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            get,
        ),
        tool(
            Spec {
                name: "editor_settings_update",
                title: "Update persisted editor settings",
                description: "Merge a validated partial settings object into project editor.json. Snapping, grid and gizmo merge field by field; bookmarks replaces the list. Existing files require expected_revision from editor_settings_get. Missing files use defaults and require expected_revision omitted. Rejected edits preserve existing bytes. Running editors see persisted changes after reopening; their unsaved view settings are not changed.",
                input: schema::object(
                    vec![
                        (
                            "patch",
                            json!({"type":"object","description":"Partial shared EditorSettings document. Unknown fields, invalid ranges and bookmarks are rejected."}),
                        ),
                        (
                            "expected_revision",
                            schema::string(
                                "Required for an existing file; omit when creating missing settings.",
                            ),
                        ),
                    ],
                    &["patch"],
                ),
                output: output(),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            update,
        ),
    ]
}
struct Loaded {
    target: paths::WriteTarget,
    bytes: Option<Vec<u8>>,
    settings: EditorSettings,
}
fn invalid(error: impl std::fmt::Display) -> ToolFailure {
    ToolFailure::new("editor.settings.invalid", error.to_string())
}
fn too_large() -> ToolFailure {
    ToolFailure::new(
        "editor.settings.too_large",
        format!("Editor settings exceed {MAX_DOCUMENT_BYTES} bytes"),
    )
}
fn read(path: &std::path::Path) -> Result<Vec<u8>, ToolFailure> {
    paths::read_bounded(path, MAX_DOCUMENT_BYTES as usize).map_err(|error| match error {
        paths::ReadError::TooLarge => too_large(),
        paths::ReadError::Io => ToolFailure::new("io_error", "Editor settings could not be read"),
    })
}
fn load(cx: &ToolContext<'_>) -> Result<Loaded, ToolFailure> {
    // Use the no-link resolver even for reads; no caller-controlled path is accepted.
    let target = paths::resolve_write_target(cx.root, SETTINGS_FILE, "settings")?;
    let bytes = if target.exists {
        Some(read(&target.path)?)
    } else {
        None
    };
    let settings = match &bytes {
        Some(bytes) => EditorSettings::parse(bytes).map_err(invalid)?,
        None => EditorSettings::default(),
    };
    Ok(Loaded {
        target,
        bytes,
        settings,
    })
}
fn result(settings: &EditorSettings, bytes: Option<&[u8]>) -> ToolOutcome {
    Ok(ToolResult::new(json!({"path":SETTINGS_FILE,"exists":bytes.is_some(),"revision":bytes.map(paths::content_hash),"settings":clean(settings)})).verbatim())
}
fn get(cx: &ToolContext<'_>, _: &Map<String, Value>) -> ToolOutcome {
    let loaded = load(cx)?;
    result(&loaded.settings, loaded.bytes.as_deref())
}
fn conflict() -> ToolFailure {
    ToolFailure::new(
        "revision_conflict",
        "Editor settings changed; read them again before updating",
    )
}
fn update(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    cx.require(AccessLevel::Write)?;
    let args = Args(args);
    let expected = args.optional_string("expected_revision")?;
    let patch = args
        .raw("patch")
        .filter(|v| v.is_object())
        .ok_or_else(|| schema::invalid("patch", "a settings object"))?;
    if serde_json::to_vec(patch).map_err(invalid)?.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    let loaded = load(cx)?;
    match (&loaded.bytes, expected) {
        (Some(_), None) => {
            return Err(ToolFailure::new(
                "revision_required",
                "Existing settings require expected_revision from editor_settings_get",
            ));
        }
        (Some(bytes), Some(expected)) if paths::content_hash(bytes) != expected => {
            return Err(conflict());
        }
        (None, Some(_)) => return Err(conflict()),
        _ => {}
    }
    let settings = loaded.settings.merged(patch).map_err(invalid)?;
    let text = settings.to_json().map_err(invalid)?;
    if text.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    // Match graph_edit's final confinement and exact-byte recheck before atomic publish.
    // This narrows the race window; it is not a cross-process compare-and-swap.
    let target = paths::resolve_write_target(cx.root, SETTINGS_FILE, "settings")?;
    if target.path != loaded.target.path || target.exists != loaded.bytes.is_some() {
        return Err(conflict());
    }
    if let Some(previous) = &loaded.bytes {
        if read(&target.path)? != *previous {
            return Err(conflict());
        }
        paths::atomic_write(&target.path, text.as_bytes()).map_err(|error| {
            ToolFailure::new(
                "io_error",
                format!("Editor settings could not be written: {}", error.kind()),
            )
        })?;
    } else {
        publish_new_file(
            cx,
            SETTINGS_FILE,
            text.as_bytes(),
            "editor.settings.publish",
        )?;
    }
    result(&settings, Some(text.as_bytes()))
}
