//! UI authoring tools (`docs/ui-authoring.md`): `ui_inspect` (computed layout,
//! hit test and layout issues of an entity's `engine.UiRoot`), `ui_preview` (a
//! rasterized PNG) and `ui_edit` (transactional document operations).
//!
//! The reads never run game code: they lay the authored tree out and rasterize
//! it with the headless preview pipeline of engine-project. `ui_edit` applies
//! the shared [`UiOp`] vocabulary to a copy of the root, validates the result
//! and saves it through the same revisioned scene-edit path as `component_set`,
//! so a rejected edit changes nothing on disk.
use super::{
    Spec,
    edit::{apply_edit, find},
    scene::{self, Loaded},
    tool,
};
use crate::{
    config::AccessLevel,
    registry::{Annotations, Content, Tool, ToolContext, ToolFailure, ToolOutcome, ToolResult},
    schema::{self, Args},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use engine_project::{
    EditOperation, Scene, SceneEntity,
    ui_tree::{
        MAX_RASTER_DIMENSION, NullTextBackend, UiComposedTheme, UiHitMode, UiOp, UiPreviewAssets,
        UiPreviewOptions, UiRoot, UiSampling, UiStateFlags, UiTextBackend, UiTheme, UiThemeSet,
        UiViewOptions, apply_ops, diff_roots, effective_root, inspect_layout, referenced_textures,
        render_preview, state_flag,
    },
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const COMPONENT: &str = "engine.UiRoot";
const THEME_COMPONENT: &str = "engine.UiTheme";
const DEFAULT_VIEWPORT: [u32; 2] = [1280, 720];
/// Pixels a returned preview image may have.
const MAX_PREVIEW_PIXELS: u64 = 2048 * 2048;
/// PNG bytes a returned preview may have.
const MAX_PREVIEW_BYTES: usize = 6 * 1024 * 1024;
const STATE_FLAGS: [&str; 7] = [
    "hovered", "pressed", "focused", "disabled", "checked", "open", "selected",
];

fn number(description: &str, minimum: f64, maximum: f64, default: f64) -> Value {
    json!({"type": "number", "minimum": minimum, "maximum": maximum, "default": default,
        "description": description})
}

fn scene_property() -> (&'static str, Value) {
    (
        "scene",
        schema::string("Project-relative scene path (see scene_list)."),
    )
}

fn entity_property() -> (&'static str, Value) {
    (
        "entity",
        schema::string("ID of the entity that holds the engine.UiRoot component."),
    )
}

/// Properties shared by `ui_inspect` and `ui_preview`.
fn view_properties() -> Vec<(&'static str, Value)> {
    vec![
        scene_property(),
        entity_property(),
        (
            "viewport",
            json!({"type": "array", "minItems": 2, "maxItems": 2,
                "items": {"type": "integer", "minimum": 1, "maximum": 16384},
                "description": "Layout viewport [width, height] in logical pixels (default [1280, 720])."}),
        ),
        (
            "theme",
            schema::string(
                "Name of an engine.UiTheme entity in the scene to cascade over the tree. \
                 Defaults to the root's own `theme`; an empty string draws without a theme.",
            ),
        ),
        (
            "locale",
            schema::string(
                "Locale to resolve @message.key texts in (needs a locale_config asset); \
                 defaults to the project's default locale.",
            ),
        ),
        (
            "states",
            json!({"type": "object",
                "additionalProperties": {"type": "array", "items": {"type": "string", "enum": STATE_FLAGS}},
                "description": "Interaction flags by node ID, for example {\"play\": [\"hovered\"]}. \
                    They select :hover/:pressed/:focus/... theme rules and the focus ring."}),
        ),
        (
            "font",
            schema::string(
                "Font key (bitmap font or font_face family) for nodes that name none. Defaults \
                 to the project's only font.",
            ),
        ),
    ]
}

pub(super) fn tools() -> Vec<Box<dyn Tool>> {
    let mut inspect = view_properties();
    inspect.extend([
        (
            "point",
            json!({"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number"},
                "description": "Hit test this [x, y] point (logical pixels): returns the topmost \
                    node and the stack under it."}),
        ),
        (
            "offset",
            schema::integer(
                "Nodes to skip in paint order (paging).",
                0,
                100_000,
                Some(0),
            ),
        ),
        (
            "limit",
            schema::integer("Most nodes returned.", 0, 1024, Some(300)),
        ),
    ]);
    let mut preview = view_properties();
    preview.extend([
        (
            "scale",
            number(
                "Image pixels per logical pixel; the image is at most 2048x2048.",
                0.1,
                8.0,
                1.0,
            ),
        ),
        (
            "background",
            json!({"type": "array", "minItems": 4, "maxItems": 4,
                "items": {"type": "number", "minimum": 0, "maximum": 1},
                "description": "Straight RGBA behind the UI (default a dark grey)."}),
        ),
        (
            "sampling",
            schema::string_enum(
                "Texture filter: `nearest` matches the engine's GPU sampler.",
                &["nearest", "bilinear"],
            ),
        ),
    ]);
    let edit = vec![
        scene_property(),
        (
            "expected_revision",
            schema::string(
                "`revision` from scene_inspect, entity_get, ui_inspect or the previous edit. \
                 The edit is rejected with revision_conflict when the scene changed since.",
            ),
        ),
        entity_property(),
        (
            "operations",
            json!({"type": "array", "maxItems": 1024, "items": {"type": "object"},
                "description": "Ordered UI document operations applied atomically as one step: \
                    add_node {id,widget,parent?,index?,node?}, remove_node {id}, move_node \
                    {id,parent?,index?}, duplicate_node {id,new_id?,parent?,index?}, rename_node \
                    {id,new_id}, set_widget/set_style/set_node {id,patch}, set_classes \
                    {id,classes}, set_accessibility {id,accessibility}, set_bindings {bindings}, \
                    set_binding {node,property,binding}, set_root {patch}."}),
        ),
        (
            "expected_hash",
            schema::string(
                "Optional `ui_hash` from ui_inspect; the edit is rejected with ui_conflict when \
                 the UI root changed since.",
            ),
        ),
        (
            "dry_run",
            schema::boolean(
                "Validate the operations and report the result without saving anything.",
                false,
            ),
        ),
    ];
    vec![
        tool(
            Spec {
                name: "ui_inspect",
                title: "Inspect UI layout",
                description: "Lay out the engine.UiRoot of a scene entity at a viewport and \
                    return, in paint order, every node's computed boxes (border `rect`, `content`, \
                    ancestor `clip`, `visible_rect`; [x,y,w,h] logical pixels) with its parent, \
                    children and widget kind, the nodes without a box, and layout issues \
                    (overflow, zero_size, clipped_away, unreachable_focus, nav_target_not_focusable). \
                    Optionally hit-tests a point. Applies the theme, locale and interaction \
                    states you pass; read-only and deterministic.",
                input: schema::object(inspect, &["scene", "entity"]),
                output: schema::output(vec![
                    ("scene", schema::string("Project-relative scene path.")),
                    ("entity", schema::string("Entity ID.")),
                    ("revision", schema::string("Scene revision for ui_edit.")),
                    (
                        "ui_hash",
                        schema::string("Hash of the UI root for ui_edit's expected_hash."),
                    ),
                    ("node_count", json!({"type": "integer"})),
                    (
                        "viewport",
                        json!({"type": "array", "items": {"type": "integer"}}),
                    ),
                    ("theme", json!({"type": ["string", "null"]})),
                    (
                        "text_backend",
                        schema::string("shaped (font_face), bitmap or none (text measures empty)."),
                    ),
                    ("font", json!({"type": ["string", "null"]})),
                    (
                        "nodes",
                        json!({"type": "array", "items": {"type": "object"},
                            "description": "Visible nodes with a box, in paint order."}),
                    ),
                    ("returned", json!({"type": "integer"})),
                    ("truncated", json!({"type": "boolean"})),
                    ("next_offset", json!({"type": "integer"})),
                    (
                        "unplaced",
                        json!({"type": "array", "items": {"type": "string"},
                            "description": "Nodes without a box: hidden, in an unselected tab or a closed dialog."}),
                    ),
                    (
                        "issues",
                        json!({"type": "array", "items": {"type": "object"}}),
                    ),
                    ("issues_truncated", json!({"type": "boolean"})),
                    ("hit", json!({"type": ["object", "null"]})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            ui_inspect,
        ),
        tool(
            Spec {
                name: "ui_preview",
                title: "Render UI preview",
                description: "Render the engine.UiRoot of a scene entity to a PNG with the \
                    headless CPU rasterizer and return it as MCP image content, plus its size and \
                    digest. Uses the project's bitmap fonts, font_face assets and textures, and \
                    the theme, locale and interaction states you pass. Coverage text only (no \
                    SDF, post effects or GPU features); deterministic. The image is at most \
                    2048x2048 pixels.",
                input: schema::object(preview, &["scene", "entity"]),
                output: schema::output(vec![
                    ("scene", schema::string("Project-relative scene path.")),
                    ("entity", schema::string("Entity ID.")),
                    ("revision", schema::string("Scene revision.")),
                    ("width", json!({"type": "integer"})),
                    ("height", json!({"type": "integer"})),
                    ("png_bytes", json!({"type": "integer"})),
                    (
                        "digest",
                        schema::string("BLAKE3 hex digest of the RGBA pixels."),
                    ),
                    ("draws", json!({"type": "integer"})),
                    ("triangles", json!({"type": "integer"})),
                    ("theme", json!({"type": ["string", "null"]})),
                    ("font", json!({"type": ["string", "null"]})),
                ]),
                annotations: Annotations::READ_ONLY,
                access: AccessLevel::Read,
            },
            ui_preview,
        ),
        tool(
            Spec {
                name: "ui_edit",
                title: "Edit UI document",
                description: "Atomically apply an ordered batch of UI document operations to \
                    the engine.UiRoot of a scene entity (add/remove/move/duplicate/rename nodes, \
                    patch widgets, styles, classes, accessibility, bindings and root fields). \
                    The batch is validated as a whole and saved with the scene's revision check; \
                    any failing operation or invalid result changes nothing. Returns a report per \
                    operation, the changed node IDs and the new revision and ui_hash.",
                input: schema::object(
                    edit,
                    &["scene", "expected_revision", "entity", "operations"],
                ),
                output: schema::output(vec![
                    ("scene", schema::string("Project-relative scene path.")),
                    ("entity", schema::string("Entity ID.")),
                    (
                        "saved",
                        json!({"type": "boolean", "description": "false for dry runs."}),
                    ),
                    ("dry_run", json!({"type": "boolean"})),
                    (
                        "previous_revision",
                        schema::string("Revision before the edit."),
                    ),
                    ("revision", schema::string("Revision after the edit.")),
                    (
                        "previous_ui_hash",
                        schema::string("UI hash before the edit."),
                    ),
                    ("ui_hash", schema::string("UI hash after the edit.")),
                    ("node_count", json!({"type": "integer"})),
                    (
                        "reports",
                        json!({"type": "array", "items": {"type": "object"},
                            "description": "One report per operation: op, ids and id_map."}),
                    ),
                    (
                        "diff",
                        json!({"type": "object",
                            "description": "added, removed, changed, reordered and root fields that differ."}),
                    ),
                    (
                        "changed_ids",
                        json!({"type": "array", "items": {"type": "string"}}),
                    ),
                ]),
                annotations: Annotations::mutation(true, false),
                access: AccessLevel::Write,
            },
            ui_edit,
        ),
    ]
}

// ---- shared loading ----

fn ui_failure(error: engine_project::Error) -> ToolFailure {
    let mut failure = ToolFailure::from(error);
    failure.hint = Some(
        "The UI document is unchanged. See docs/ui-authoring.md for the operation vocabulary \
         and error codes."
            .into(),
    );
    failure
}

fn ui_root_of(entity: &SceneEntity) -> Result<UiRoot, ToolFailure> {
    let Some(custom) = entity.components.custom.get(COMPONENT) else {
        return Err(ToolFailure::new(
            "ui_root_missing",
            format!("entity {} has no {COMPONENT} component", entity.id),
        )
        .with_hint(
            "Add one with component_set, for example value {\"version\":1,\"data\":{\"version\":1,\"tree\":{\"nodes\":[]}}}.",
        ));
    };
    let root: UiRoot = serde_json::from_value(custom.data.clone())
        .map_err(|e| ToolFailure::new("ui_root_invalid", e.to_string()))?;
    root.validate().map_err(ui_failure)?;
    Ok(root)
}

fn viewport_arg(a: &Args<'_>) -> Result<[u32; 2], ToolFailure> {
    let bad = || schema::invalid("viewport", "[width, height], integers from 1 to 16384");
    let Some(value) = a.raw("viewport").filter(|v| !v.is_null()) else {
        return Ok(DEFAULT_VIEWPORT);
    };
    let items = value.as_array().filter(|a| a.len() == 2).ok_or_else(bad)?;
    let side = |v: &Value| {
        v.as_u64()
            .filter(|n| (1..=16384).contains(n))
            .map(|n| n as u32)
            .ok_or_else(bad)
    };
    Ok([side(&items[0])?, side(&items[1])?])
}

fn states_arg(a: &Args<'_>, root: &UiRoot) -> Result<BTreeMap<String, UiStateFlags>, ToolFailure> {
    let mut out = BTreeMap::new();
    let Some(object) = a.optional_object("states")? else {
        return Ok(out);
    };
    for (node, flags) in object {
        if root.tree.find(node).is_none() {
            return Err(schema::invalid(
                "states",
                &format!("keyed by node IDs; `{node}` is unknown"),
            ));
        }
        let mut set = UiStateFlags::default();
        for flag in flags
            .as_array()
            .ok_or_else(|| schema::invalid("states", "an object of flag-name arrays"))?
        {
            let flag = flag.as_str().and_then(state_flag).ok_or_else(|| {
                schema::invalid("states", &format!("flags among {}", STATE_FLAGS.join(", ")))
            })?;
            set.insert(flag);
        }
        out.insert(node.clone(), set);
    }
    Ok(out)
}

/// `engine.UiTheme` components of the scene, by name.
fn scene_themes(scene: &Scene) -> Result<UiThemeSet, ToolFailure> {
    let mut set = UiThemeSet::new();
    for entity in &scene.entities {
        if let Some(custom) = entity.components.custom.get(THEME_COMPONENT) {
            let theme: UiTheme = serde_json::from_value(custom.data.clone()).map_err(|e| {
                ToolFailure::new(
                    "ui_theme_invalid",
                    format!("{THEME_COMPONENT} of entity {}: {e}", entity.id),
                )
            })?;
            set.insert(theme).map_err(ui_failure)?;
        }
    }
    Ok(set)
}

/// The composed theme named by `theme`, else by the root, else none.
fn theme_arg(
    a: &Args<'_>,
    scene: &Scene,
    root: &UiRoot,
) -> Result<(Option<String>, Option<UiComposedTheme>), ToolFailure> {
    let name = match a.optional_string("theme")? {
        Some("") => None,
        Some(name) => Some(name.to_owned()),
        None => root.theme.clone(),
    };
    let Some(name) = name else {
        return Ok((None, None));
    };
    let themes = scene_themes(scene)?;
    let composed = themes.compose(&name).map_err(|e| {
        let mut failure = ui_failure(e);
        failure.hint = Some(format!(
            "Theme `{name}` must be an engine.UiTheme entity of this scene (names: {}). \
             Pass theme \"\" to draw without a theme.",
            themes.names().collect::<Vec<_>>().join(", ")
        ));
        failure
    })?;
    Ok((Some(name), Some(composed)))
}

/// Everything a read tool needs, loaded once.
struct View {
    loaded: Loaded,
    entity: String,
    root: UiRoot,
    assets: UiPreviewAssets,
    theme_name: Option<String>,
    theme: Option<UiComposedTheme>,
    states: BTreeMap<String, UiStateFlags>,
    font: Option<String>,
}

impl View {
    fn load(cx: &ToolContext<'_>, args: &Map<String, Value>) -> Result<Self, ToolFailure> {
        let a = Args(args);
        let entity = a.string("entity")?.to_owned();
        let loaded = scene::load(cx, args)?;
        let root = ui_root_of(find(&loaded.scene, &entity, &loaded.path)?)?;
        let states = states_arg(&a, &root)?;
        let (theme_name, theme) = theme_arg(&a, &loaded.scene, &root)?;
        let assets = UiPreviewAssets::from_project(&loaded.project, &referenced_textures(&root))
            .map_err(ui_failure)?;
        if let Some(locale) = a.optional_string("locale")? {
            let localization = assets.localization.as_ref().ok_or_else(|| {
                ToolFailure::new(
                    "ui_locale_unavailable",
                    "the project has no locale_config asset to resolve a locale",
                )
            })?;
            localization.set_locale(locale).map_err(ui_failure)?;
        }
        let font = a
            .optional_string("font")?
            .map(str::to_owned)
            .or_else(|| assets.sole_font());
        Ok(Self {
            loaded,
            entity,
            root,
            assets,
            theme_name,
            theme,
            states,
            font,
        })
    }

    fn options(&self) -> UiViewOptions<'_> {
        UiViewOptions {
            theme: self.theme.as_ref(),
            states: self.states.clone(),
            localization: self.assets.localization.as_ref(),
            default_font: self.font.as_deref(),
        }
    }

    fn text_backend_name(&self) -> &'static str {
        if self.assets.engine.is_some() {
            "shaped"
        } else if !self.assets.fonts.is_empty() {
            "bitmap"
        } else {
            "none"
        }
    }
}

fn font_hint(failure: ToolFailure) -> ToolFailure {
    if failure.code.starts_with("render.font") {
        let mut failure = failure;
        failure.hint = Some(
            "A node with text names no usable font. Pass `font` (a bitmap font asset ID or a \
             font_face family) or set style.font on the node."
                .into(),
        );
        failure
    } else {
        failure
    }
}

// ---- ui_inspect ----

fn children_of(root: &UiRoot, id: &str) -> Vec<String> {
    root.tree
        .nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(id))
        .map(|n| n.id.clone())
        .collect()
}

fn ui_inspect(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let viewport = viewport_arg(&a)?;
    let offset = a.integer("offset", 0, 100_000, 0)? as usize;
    let limit = a.integer("limit", 0, 1024, 300)? as usize;
    let point = match a.raw("point").filter(|v| !v.is_null()) {
        None => None,
        Some(value) => {
            let bad = || schema::invalid("point", "[x, y], finite numbers");
            let items = value.as_array().filter(|a| a.len() == 2).ok_or_else(bad)?;
            let coordinate = |v: &Value| v.as_f64().filter(|n| n.is_finite()).ok_or_else(bad);
            Some([coordinate(&items[0])? as f32, coordinate(&items[1])? as f32])
        }
    };
    let view = View::load(cx, args)?;
    let effective =
        effective_root(&view.root, &view.entity, &view.options()).map_err(ui_failure)?;
    let selecting;
    let null = NullTextBackend;
    let backend: &dyn UiTextBackend = if view.assets.has_fonts() {
        selecting = view.assets.text();
        &selecting
    } else {
        &null
    };
    let inspection = inspect_layout(&effective, viewport, backend, &view.states)
        .map_err(|e| font_hint(ui_failure(e)))?;
    let page: Vec<Value> = inspection
        .nodes
        .iter()
        .skip(offset)
        .take(limit)
        .map(|n| {
            let mut value = n.to_json();
            value["children"] = json!(children_of(&effective, &n.id));
            value
        })
        .collect();
    let end = offset + page.len();
    let mut result = json!({
        "scene": view.loaded.path,
        "entity": view.entity,
        "revision": view.loaded.revision,
        "ui_hash": view.root.content_hash(),
        "node_count": view.root.tree.nodes.len(),
        "viewport": viewport,
        "theme": view.theme_name,
        "text_backend": view.text_backend_name(),
        "font": view.font,
        "returned": page.len(),
        "nodes": page,
        "truncated": end < inspection.nodes.len(),
        "unplaced": inspection.unplaced,
        "issues": inspection.issues,
        "issues_truncated": inspection.issues_truncated,
    });
    if end < inspection.nodes.len() {
        result["next_offset"] = json!(end);
    }
    if let Some(point) = point {
        let stack = inspection.hit_stack(point, UiHitMode::Any, &effective);
        let interactive = inspection.hit_test(point, UiHitMode::Interactive, &effective);
        result["hit"] = match stack.first() {
            Some(top) => json!({
                "point": point,
                "id": top.id,
                "interactive": interactive.map(|n| n.id.clone()),
                "stack": stack.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
            }),
            None => json!({"point": point, "id": null, "stack": []}),
        };
    }
    if backend_is_empty(&view) {
        result["warnings"] = json!([
            "The project has no fonts: text measures as empty, so content-sized text boxes are zero wide."
        ]);
    }
    Ok(ToolResult::new(result).verbatim())
}

fn backend_is_empty(view: &View) -> bool {
    !view.assets.has_fonts()
        && view.root.tree.nodes.iter().any(|n| {
            !matches!(
                n.widget,
                engine_project::ui_tree::UiWidget::Container
                    | engine_project::ui_tree::UiWidget::Image { .. }
                    | engine_project::ui_tree::UiWidget::NineSlice { .. }
            )
        })
}

// ---- ui_preview ----

fn ui_preview(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let viewport = viewport_arg(&a)?;
    let scale = match a.raw("scale").filter(|v| !v.is_null()) {
        None => 1.0,
        Some(v) => v
            .as_f64()
            .filter(|s| (0.1..=8.0).contains(s))
            .ok_or_else(|| schema::invalid("scale", "a number from 0.1 to 8"))?
            as f32,
    };
    let clear = match a.raw("background").filter(|v| !v.is_null()) {
        None => UiPreviewOptions::new(viewport).clear,
        Some(value) => {
            let bad = || schema::invalid("background", "[r, g, b, a], numbers from 0 to 1");
            let items = value.as_array().filter(|a| a.len() == 4).ok_or_else(bad)?;
            let mut out = [0.0_f32; 4];
            for (slot, item) in out.iter_mut().zip(items) {
                *slot = item
                    .as_f64()
                    .filter(|n| (0.0..=1.0).contains(n))
                    .ok_or_else(bad)? as f32;
            }
            out
        }
    };
    let sampling = match a.one_of("sampling", &["nearest", "bilinear"])? {
        Some("bilinear") => UiSampling::Bilinear,
        _ => UiSampling::Nearest,
    };
    let pixels: u64 = viewport
        .iter()
        .map(|side| (f64::from(*side) * f64::from(scale)).ceil() as u64)
        .product();
    let sides_ok = viewport.iter().all(|side| {
        (f64::from(*side) * f64::from(scale)).ceil() <= f64::from(MAX_RASTER_DIMENSION)
    });
    if pixels > MAX_PREVIEW_PIXELS || !sides_ok {
        return Err(ToolFailure::new(
            "ui.preview.size",
            format!(
                "the preview would be larger than {MAX_PREVIEW_PIXELS} pixels; lower `viewport` or `scale`"
            ),
        )
        .with_hint("Previews are at most 2048x2048 pixels (viewport times scale)."));
    }
    let view = View::load(cx, args)?;
    let options = UiPreviewOptions {
        view: view.options(),
        viewport,
        scale,
        clear,
        sampling,
    };
    let preview = render_preview(&view.root, &view.entity, &view.assets, &options)
        .map_err(|e| font_hint(ui_failure(e)))?;
    let png = preview.image.to_png().map_err(ui_failure)?;
    if png.len() > MAX_PREVIEW_BYTES {
        return Err(ToolFailure::new(
            "ui.preview.size",
            "the encoded preview exceeds 6 MiB; lower `viewport` or `scale`",
        ));
    }
    let result = json!({
        "scene": view.loaded.path,
        "entity": view.entity,
        "revision": view.loaded.revision,
        "width": preview.image.width,
        "height": preview.image.height,
        "png_bytes": png.len(),
        "digest": preview.image.digest(),
        "draws": preview.draws,
        "triangles": preview.triangles,
        "theme": view.theme_name,
        "font": view.font,
    });
    Ok(ToolResult::new(result).with_content(Content::Image {
        data: STANDARD.encode(&png),
        mime_type: "image/png".into(),
    }))
}

// ---- ui_edit ----

fn ui_edit(cx: &ToolContext<'_>, args: &Map<String, Value>) -> ToolOutcome {
    let a = Args(args);
    let entity_id = a.string("entity")?.to_owned();
    let expected_hash = a.optional_string("expected_hash")?.map(str::to_owned);
    let operations = args
        .get("operations")
        .ok_or_else(|| schema::invalid("operations", "an array of operation objects"))?;
    let ops: Vec<UiOp> = UiOp::parse_all(operations).map_err(ui_failure)?;
    let mut summary: Option<Value> = None;
    let mut outcome = apply_edit(cx, args, |loaded| {
        let root = ui_root_of(find(&loaded.scene, &entity_id, &loaded.path)?)?;
        let before = root.content_hash();
        if let Some(expected) = &expected_hash
            && *expected != before
        {
            return Err(ToolFailure::new(
                "ui_conflict",
                "the UI root changed since the supplied expected_hash",
            )
            .with_hint(
                "Re-read it with ui_inspect, reapply your change and retry with the ui_hash it returns.",
            ));
        }
        let (next, reports) = apply_ops(&root, &ops).map_err(ui_failure)?;
        let diff = diff_roots(&root, &next);
        summary = Some(json!({
            "entity": entity_id,
            "previous_ui_hash": before,
            "ui_hash": next.content_hash(),
            "node_count": next.tree.nodes.len(),
            "reports": reports,
            "changed_ids": diff.changed_ids(),
            "diff": diff,
        }));
        Ok((
            EditOperation::SetComponent {
                entity_id: entity_id.clone(),
                component: COMPONENT.to_owned(),
                value: json!({"version": 1, "data": next.to_json()}),
                merge: false,
            },
            Some(entity_id.clone()),
        ))
    })?;
    if let (Some(summary), Some(data)) = (summary, outcome.data.as_object_mut()) {
        // The entity echo of the generic edit is large; the report replaces it.
        data.remove("entity");
        if let Some(extra) = summary.as_object() {
            data.extend(extra.clone());
        }
        data.insert("entity".into(), json!(entity_id));
    }
    Ok(outcome.verbatim())
}
