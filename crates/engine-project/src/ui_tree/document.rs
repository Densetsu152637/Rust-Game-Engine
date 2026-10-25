//! UI authoring document: one [`UiRoot`] (plus an optional theme) edited
//! through a small, serializable operation vocabulary (`docs/ui-authoring.md`).
//!
//! The editor domain and the MCP `ui_edit` tool share [`UiOp`]. A batch of
//! operations is applied to a copy of the root, validated once as a whole and
//! then committed as ONE undo step; a failing operation or an invalid result
//! leaves the document untouched. Undo and redo restore stored snapshots, so
//! the serialized root is byte-identical to the state before the edit.
use super::*;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Operations accepted per batch.
pub const MAX_UI_OPS: usize = 1024;
/// Serialized size limit of one patch, widget spec or binding.
pub const MAX_UI_PATCH_BYTES: usize = 65536;
/// Undo steps kept by default.
pub const DEFAULT_UI_HISTORY: usize = 128;
/// Largest configurable undo depth.
pub const MAX_UI_HISTORY: usize = 1024;
/// Change records kept for [`UiDocument::take_changes`] before the oldest drop.
const MAX_PENDING_CHANGES: usize = 256;

/// `kind` values of [`UiWidget`], in declaration order.
pub const UI_WIDGET_KINDS: [&str; 15] = [
    "container",
    "label",
    "rich_label",
    "image",
    "nine_slice",
    "button",
    "toggle",
    "slider",
    "progress",
    "text_input",
    "dropdown",
    "scroll_view",
    "list",
    "virtual_list",
    "tabs",
];

/// Keys [`UiOp::SetNode`] may patch.
const NODE_PATCH_KEYS: [&str; 4] = ["visible", "enabled", "tooltip", "nav"];
/// Keys [`UiOp::SetRoot`] may patch.
const ROOT_PATCH_KEYS: [&str; 5] = ["layer", "visible", "enabled", "theme", "reducedMotion"];

fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// One document edit. Serialized as `{"op": "<snake_case name>", ...}`.
///
/// Patches are JSON merge patches (RFC 7386 with one rule: an object that
/// carries a `kind` or `unit` discriminator replaces instead of merging, so
/// tagged values such as `{"unit":"percent","value":0.5}` stay well formed).
/// `null` resets a field to its default.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiOp {
    /// Insert a node. `widget` is a kind name (`"button"`) or an object with a
    /// `kind` plus fields laid over that kind's defaults. `node` may carry
    /// `style`, `visible`, `enabled`, `classes`, `tooltip`, `nav`, `accessibility`.
    AddNode {
        id: String,
        #[serde(default)]
        widget: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Position among the siblings; absent appends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<Map<String, Value>>,
    },
    /// Remove a node and its subtree, with the bindings and navigation
    /// overrides that referred to it.
    RemoveNode { id: String },
    /// Reparent and/or reorder a node with its subtree. `parent` absent keeps
    /// the parent, `null` makes the node top level. `index` is the final
    /// position among the new siblings.
    MoveNode {
        id: String,
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        parent: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Copy a node and its subtree with fresh IDs (`<id>_copy`, `<id>_copy2`,
    /// ...). References inside the copy (navigation, virtual-list template,
    /// bindings) point at the copy. The copy follows the original by default.
    DuplicateNode {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        parent: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Change a node ID and every reference to it (children's `parent`,
    /// navigation overrides, virtual-list `template`, bindings).
    RenameNode { id: String, new_id: String },
    /// Patch the widget. A `kind` that differs from the current one switches
    /// the widget to that kind's defaults before the patch applies.
    SetWidget {
        id: String,
        patch: Map<String, Value>,
    },
    /// Patch the node style (fields of [`UiStyle`], camelCase).
    SetStyle {
        id: String,
        patch: Map<String, Value>,
    },
    /// Patch `visible`, `enabled`, `tooltip` or `nav`.
    SetNode {
        id: String,
        patch: Map<String, Value>,
    },
    /// Replace the class list.
    SetClasses { id: String, classes: Vec<String> },
    /// Replace the accessibility overrides; `null` clears them.
    SetAccessibility {
        id: String,
        accessibility: Option<Value>,
    },
    /// Replace all data bindings of the root.
    SetBindings { bindings: Vec<UiBinding> },
    /// Upsert (object) or remove (`null`) the binding of one node property.
    /// The object holds the other [`UiBinding`] fields (`path`, `mode`, ...).
    SetBinding {
        node: String,
        property: UiBindProperty,
        binding: Option<Map<String, Value>>,
    },
    /// Patch `layer`, `visible`, `enabled`, `theme` or `reducedMotion`.
    SetRoot { patch: Map<String, Value> },
}

impl UiOp {
    /// The `op` value.
    pub fn name(&self) -> &'static str {
        match self {
            Self::AddNode { .. } => "add_node",
            Self::RemoveNode { .. } => "remove_node",
            Self::MoveNode { .. } => "move_node",
            Self::DuplicateNode { .. } => "duplicate_node",
            Self::RenameNode { .. } => "rename_node",
            Self::SetWidget { .. } => "set_widget",
            Self::SetStyle { .. } => "set_style",
            Self::SetNode { .. } => "set_node",
            Self::SetClasses { .. } => "set_classes",
            Self::SetAccessibility { .. } => "set_accessibility",
            Self::SetBindings { .. } => "set_bindings",
            Self::SetBinding { .. } => "set_binding",
            Self::SetRoot { .. } => "set_root",
        }
    }

    /// Parse an array of operations (see [`MAX_UI_OPS`]).
    pub fn parse_all(value: &Value) -> Result<Vec<UiOp>> {
        let items = value
            .as_array()
            .ok_or_else(|| op_error("ui.op.invalid", "Operations must be a JSON array"))?;
        if items.len() > MAX_UI_OPS {
            return Err(op_error(
                "ui.op.limit",
                format!("At most {MAX_UI_OPS} operations per batch"),
            ));
        }
        items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                serde_json::from_value::<UiOp>(item.clone())
                    .map_err(|e| op_error("ui.op.invalid", format!("op {i}: {e}")))
            })
            .collect()
    }
}

/// What one applied operation did.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UiOpReport {
    pub op: String,
    /// Node IDs the operation created or edited (the new IDs after a rename).
    pub ids: Vec<String>,
    /// Old to new ID for `rename_node` and `duplicate_node`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub id_map: BTreeMap<String, String>,
}

fn op_error(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn unknown(id: &str) -> Error {
    op_error("ui.op.unknown_node", format!("No UI node with ID `{id}`"))
}

/// Prefix an error with the failing operation, keeping its code.
fn context(error: Error, index: usize, name: &str) -> Error {
    let (code, message) = error
        .diagnostics
        .first()
        .map(|d| (d.code.clone(), d.message.clone()))
        .unwrap_or_else(|| ("ui.op.invalid".into(), "operation failed".into()));
    Error::new(&code, format!("op {index} ({name}): {message}"))
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(op_error(
            "ui.op.invalid_id",
            "A node ID is 1 to 128 bytes long",
        ));
    }
    Ok(())
}

fn position(nodes: &[UiNode], id: &str) -> Result<usize> {
    nodes
        .iter()
        .position(|n| n.id == id)
        .ok_or_else(|| unknown(id))
}

fn check_patch(patch: &Map<String, Value>) -> Result<()> {
    let size = serde_json::to_vec(patch)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if size > MAX_UI_PATCH_BYTES {
        return Err(op_error("ui.op.limit", "A patch is limited to 64 KiB"));
    }
    Ok(())
}

/// Tagged values replace instead of merging.
fn discriminated(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.contains_key("kind") || o.contains_key("unit"))
}

/// JSON merge patch (see [`UiOp`]).
pub(super) fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Some(object) = target.as_object_mut() else {
        return;
    };
    for (key, value) in patch {
        if value.is_null() {
            object.remove(key);
        } else if value.is_object()
            && !discriminated(value)
            && object.get(key).is_some_and(Value::is_object)
        {
            if let Some(slot) = object.get_mut(key) {
                merge_patch(slot, value);
            }
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
}

/// The merge patch that turns `a` into `b` (objects), the inverse of [`merge_patch`].
pub(super) fn make_patch(a: &Value, b: &Value) -> Value {
    let (Value::Object(old), Value::Object(new)) = (a, b) else {
        return b.clone();
    };
    let mut patch = Map::new();
    for key in old.keys().filter(|k| !new.contains_key(*k)) {
        patch.insert(key.clone(), Value::Null);
    }
    for (key, value) in new {
        match old.get(key) {
            Some(previous) if previous == value => {}
            Some(previous)
                if previous.is_object()
                    && value.is_object()
                    && !discriminated(previous)
                    && !discriminated(value) =>
            {
                patch.insert(key.clone(), make_patch(previous, value));
            }
            _ => {
                patch.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(patch)
}

/// Required fields of each kind with placeholder values; everything else
/// takes its serde default, so serialized widgets stay minimal.
fn default_widget_json(kind: &str) -> Result<Value> {
    Ok(match kind {
        "container" => json!({"kind": "container"}),
        "label" => json!({"kind": "label", "text": "Label"}),
        "rich_label" => json!({"kind": "rich_label", "markup": "Rich text"}),
        "image" => json!({"kind": "image", "texture": ""}),
        "nine_slice" => json!({"kind": "nine_slice", "texture": "", "slices": [0, 0, 0, 0]}),
        "button" => json!({"kind": "button", "text": "Button"}),
        "toggle" => json!({"kind": "toggle", "text": "Toggle", "checked": false}),
        "slider" => json!({"kind": "slider", "value": 0, "min": 0, "max": 1, "step": 0.01}),
        "progress" => json!({"kind": "progress", "value": 0, "min": 0, "max": 1}),
        "text_input" => json!({"kind": "text_input"}),
        "dropdown" => json!({"kind": "dropdown", "options": []}),
        "scroll_view" => json!({"kind": "scroll_view"}),
        "list" => json!({"kind": "list"}),
        "virtual_list" => {
            json!({"kind": "virtual_list", "itemCount": 0, "itemExtent": 32, "template": ""})
        }
        "tabs" => json!({"kind": "tabs", "labels": ["Tab 1"]}),
        "modal_dialog" => json!({"kind": "modal_dialog"}),
        other => {
            return Err(op_error(
                "ui.op.widget",
                format!("Unknown widget kind `{other}`"),
            ));
        }
    })
}

fn widget_from_json(value: Value) -> Result<UiWidget> {
    serde_json::from_value(value).map_err(|e| op_error("ui.op.widget", e.to_string()))
}

fn widget_from_spec(spec: &Value) -> Result<UiWidget> {
    let (kind, fields) = match spec {
        Value::Null => ("container".to_owned(), Map::new()),
        Value::String(kind) => (kind.clone(), Map::new()),
        Value::Object(object) => {
            check_patch(object)?;
            let kind = object
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| op_error("ui.op.widget", "A widget object needs a `kind`"))?;
            let mut fields = object.clone();
            fields.remove("kind");
            (kind.to_owned(), fields)
        }
        _ => {
            return Err(op_error(
                "ui.op.widget",
                "`widget` is a kind name or an object with a `kind`",
            ));
        }
    };
    let mut base = default_widget_json(&kind)?;
    merge_patch(&mut base, &Value::Object(fields));
    widget_from_json(base)
}

impl UiWidget {
    /// The serialized `kind` of this widget.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Label { .. } => "label",
            Self::RichLabel { .. } => "rich_label",
            Self::Image { .. } => "image",
            Self::NineSlice { .. } => "nine_slice",
            Self::Button { .. } => "button",
            Self::Toggle { .. } => "toggle",
            Self::Slider { .. } => "slider",
            Self::Progress { .. } => "progress",
            Self::TextInput { .. } => "text_input",
            Self::Dropdown { .. } => "dropdown",
            Self::ScrollView { .. } => "scroll_view",
            Self::List { .. } => "list",
            Self::VirtualList { .. } => "virtual_list",
            Self::Tabs { .. } => "tabs",
            Self::ModalDialog { .. } => "modal_dialog",
        }
    }
}

impl UiRoot {
    /// The root as component JSON (`engine.UiRoot` data): default values are
    /// omitted and `f32` fields print as their shortest decimal.
    pub fn to_json(&self) -> Value {
        serde_json::to_string(self)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    }

    /// Hex BLAKE3 digest of the component JSON; equal roots have equal
    /// hashes, so it detects stale edits.
    pub fn content_hash(&self) -> String {
        blake3::hash(&serde_json::to_vec(self).unwrap_or_default())
            .to_hex()
            .to_string()
    }
}

// ---- tree helpers ----

/// Vector indices of `index` and its descendants, in vector order.
fn subtree(nodes: &[UiNode], index: usize) -> Vec<usize> {
    let mut members: BTreeSet<&str> = BTreeSet::from([nodes[index].id.as_str()]);
    loop {
        let before = members.len();
        for node in nodes {
            if node.parent.as_deref().is_some_and(|p| members.contains(p)) {
                members.insert(&node.id);
            }
        }
        if members.len() == before {
            break;
        }
    }
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| members.contains(n.id.as_str()))
        .map(|(i, _)| i)
        .collect()
}

/// Vector indices of the children of `parent` (`None`: top level), in order.
fn children_of(nodes: &[UiNode], parent: Option<&str>) -> Vec<usize> {
    nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent.as_deref() == parent)
        .map(|(i, _)| i)
        .collect()
}

/// Insert `block` so its first node lands at sibling `index` of `parent`.
fn insert_block(
    nodes: &mut Vec<UiNode>,
    block: Vec<UiNode>,
    parent: Option<&str>,
    index: Option<usize>,
) -> Result<()> {
    let siblings = children_of(nodes, parent);
    let at = match index {
        Some(i) if i > siblings.len() => {
            return Err(op_error(
                "ui.op.index",
                format!("Index {i} is past the {} siblings", siblings.len()),
            ));
        }
        Some(i) if i < siblings.len() => siblings[i],
        _ => match (siblings.last(), parent) {
            (Some(last), _) => subtree(nodes, *last).last().copied().unwrap_or(*last) + 1,
            (None, Some(parent)) => position(nodes, parent)? + 1,
            (None, None) => nodes.len(),
        },
    };
    nodes.splice(at..at, block);
    if nodes.len() > MAX_UI_NODES {
        return Err(op_error(
            "ui.op.limit",
            format!("A UI root holds at most {MAX_UI_NODES} nodes"),
        ));
    }
    Ok(())
}

fn fresh_id(base: &str, taken: &BTreeSet<String>) -> String {
    for n in 1.. {
        let suffix = if n == 1 {
            "_copy".to_owned()
        } else {
            format!("_copy{n}")
        };
        let stem = &base[..base.floor_char_boundary(128 - suffix.len())];
        let candidate = format!("{stem}{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unused copy suffix exists")
}

fn nav_targets_mut(node: &mut UiNode) -> [&mut Option<String>; 4] {
    let nav = &mut node.nav;
    [&mut nav.up, &mut nav.down, &mut nav.left, &mut nav.right]
}

// ---- operation application ----

fn apply_op(root: &mut UiRoot, op: &UiOp) -> Result<UiOpReport> {
    let mut report = UiOpReport {
        op: op.name().to_owned(),
        ..Default::default()
    };
    match op {
        UiOp::AddNode {
            id,
            widget,
            parent,
            index,
            node,
        } => {
            check_id(id)?;
            let nodes = &mut root.tree.nodes;
            if nodes.iter().any(|n| n.id == *id) {
                return Err(op_error(
                    "ui.op.duplicate_id",
                    format!("A node with ID `{id}` exists"),
                ));
            }
            if let Some(parent) = parent {
                position(nodes, parent)?;
            }
            let widget = widget_from_spec(widget)?;
            let mut object = node.clone().unwrap_or_default();
            check_patch(&object)?;
            if let Some(key) = ["id", "parent", "widget"]
                .into_iter()
                .find(|k| object.contains_key(*k))
            {
                return Err(op_error(
                    "ui.op.field",
                    format!("`node.{key}` is set through the operation's own field"),
                ));
            }
            object.insert("id".into(), json!(id));
            if let Some(parent) = parent {
                object.insert("parent".into(), json!(parent));
            }
            object.insert(
                "widget".into(),
                serde_json::to_value(&widget)
                    .map_err(|e| op_error("ui.op.widget", e.to_string()))?,
            );
            let node: UiNode = serde_json::from_value(Value::Object(object))
                .map_err(|e| op_error("ui.op.node", e.to_string()))?;
            insert_block(nodes, vec![node], parent.as_deref(), *index)?;
            report.ids = vec![id.clone()];
        }
        UiOp::RemoveNode { id } => {
            let nodes = &mut root.tree.nodes;
            let members: BTreeSet<String> = subtree(nodes, position(nodes, id)?)
                .into_iter()
                .map(|i| nodes[i].id.clone())
                .collect();
            nodes.retain(|n| !members.contains(&n.id));
            for node in nodes.iter_mut() {
                for target in nav_targets_mut(node) {
                    if target.as_ref().is_some_and(|t| members.contains(t)) {
                        *target = None;
                    }
                }
            }
            root.bindings.retain(|b| !members.contains(&b.node));
            report.ids = members.into_iter().collect();
        }
        UiOp::MoveNode { id, parent, index } => {
            if parent.is_none() && index.is_none() {
                return Err(op_error(
                    "ui.op.invalid",
                    "move_node needs `parent` and/or `index`",
                ));
            }
            let nodes = &mut root.tree.nodes;
            let at = position(nodes, id)?;
            let members = subtree(nodes, at);
            let new_parent: Option<String> = match parent {
                None => nodes[at].parent.clone(),
                Some(None) => None,
                Some(Some(parent)) => {
                    let target = position(nodes, parent)?;
                    if members.contains(&target) {
                        return Err(op_error(
                            "ui.op.cycle",
                            format!("Cannot move `{id}` into itself or its own subtree"),
                        ));
                    }
                    Some(parent.clone())
                }
            };
            let member_set: BTreeSet<usize> = members.iter().copied().collect();
            let mut block = Vec::with_capacity(members.len());
            let mut rest = Vec::with_capacity(nodes.len() - members.len());
            for (i, node) in std::mem::take(nodes).into_iter().enumerate() {
                if member_set.contains(&i) {
                    block.push(node);
                } else {
                    rest.push(node);
                }
            }
            *nodes = rest;
            if let Some(moved) = block.iter_mut().find(|n| n.id == *id) {
                moved.parent = new_parent.clone();
            }
            // The moved node must lead the block so `index` addresses it.
            if let Some(first) = block.iter().position(|n| n.id == *id) {
                block.swap(0, first);
            }
            insert_block(nodes, block, new_parent.as_deref(), *index)?;
            report.ids = vec![id.clone()];
        }
        UiOp::DuplicateNode {
            id,
            new_id,
            parent,
            index,
        } => duplicate(
            root,
            id,
            new_id.as_deref(),
            parent.as_ref(),
            *index,
            &mut report,
        )?,
        UiOp::RenameNode { id, new_id } => {
            check_id(new_id)?;
            let nodes = &mut root.tree.nodes;
            position(nodes, id)?;
            if id == new_id {
                return Err(op_error("ui.op.invalid", "The new ID equals the old one"));
            }
            if nodes.iter().any(|n| n.id == *new_id) {
                return Err(op_error(
                    "ui.op.duplicate_id",
                    format!("A node with ID `{new_id}` exists"),
                ));
            }
            for node in nodes.iter_mut() {
                if node.id == *id {
                    node.id = new_id.clone();
                }
                if node.parent.as_deref() == Some(id) {
                    node.parent = Some(new_id.clone());
                }
                for target in nav_targets_mut(node) {
                    if target.as_deref() == Some(id) {
                        *target = Some(new_id.clone());
                    }
                }
                if let UiWidget::VirtualList { template, .. } = &mut node.widget
                    && template == id
                {
                    *template = new_id.clone();
                }
            }
            for binding in &mut root.bindings {
                if binding.node == *id {
                    binding.node = new_id.clone();
                }
            }
            report.ids = vec![new_id.clone()];
            report.id_map.insert(id.clone(), new_id.clone());
        }
        UiOp::SetWidget { id, patch } => {
            check_patch(patch)?;
            let node = node_mut(root, id)?;
            let current = serde_json::to_value(&node.widget)
                .map_err(|e| op_error("ui.op.widget", e.to_string()))?;
            let mut patch = patch.clone();
            let base = match patch.remove("kind") {
                Some(Value::String(kind)) if current["kind"] != json!(kind) => {
                    default_widget_json(&kind)?
                }
                Some(Value::String(_)) | None => current,
                Some(_) => return Err(op_error("ui.op.widget", "`kind` is a string")),
            };
            let mut merged = base;
            merge_patch(&mut merged, &Value::Object(patch));
            node.widget = widget_from_json(merged)?;
            report.ids = vec![id.clone()];
        }
        UiOp::SetStyle { id, patch } => {
            check_patch(patch)?;
            let node = node_mut(root, id)?;
            let mut style = serde_json::to_value(&node.style)
                .map_err(|e| op_error("ui.op.style", e.to_string()))?;
            merge_patch(&mut style, &Value::Object(patch.clone()));
            node.style = serde_json::from_value(style)
                .map_err(|e| op_error("ui.op.style", e.to_string()))?;
            report.ids = vec![id.clone()];
        }
        UiOp::SetNode { id, patch } => {
            check_patch(patch)?;
            if let Some(key) = patch
                .keys()
                .find(|k| !NODE_PATCH_KEYS.contains(&k.as_str()))
            {
                return Err(op_error(
                    "ui.op.field",
                    format!(
                        "set_node cannot patch `{key}` (allowed: {})",
                        NODE_PATCH_KEYS.join(", ")
                    ),
                ));
            }
            let node = node_mut(root, id)?;
            let mut value =
                serde_json::to_value(&*node).map_err(|e| op_error("ui.op.node", e.to_string()))?;
            merge_patch(&mut value, &Value::Object(patch.clone()));
            *node =
                serde_json::from_value(value).map_err(|e| op_error("ui.op.node", e.to_string()))?;
            report.ids = vec![id.clone()];
        }
        UiOp::SetClasses { id, classes } => {
            node_mut(root, id)?.classes = classes.clone();
            report.ids = vec![id.clone()];
        }
        UiOp::SetAccessibility { id, accessibility } => {
            let parsed = match accessibility {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    serde_json::from_value::<UiAccessibility>(value.clone())
                        .map_err(|e| op_error("ui.op.accessibility", e.to_string()))?,
                ),
            };
            node_mut(root, id)?.accessibility = parsed;
            report.ids = vec![id.clone()];
        }
        UiOp::SetBindings { bindings } => {
            root.bindings = bindings.clone();
        }
        UiOp::SetBinding {
            node,
            property,
            binding,
        } => {
            let slot = root
                .bindings
                .iter()
                .position(|b| b.node == *node && b.property == *property);
            match binding {
                None => {
                    if let Some(slot) = slot {
                        root.bindings.remove(slot);
                    }
                }
                Some(fields) => {
                    check_patch(fields)?;
                    let mut object = fields.clone();
                    object.insert("node".into(), json!(node));
                    object.insert(
                        "property".into(),
                        serde_json::to_value(property)
                            .map_err(|e| op_error("ui.op.binding", e.to_string()))?,
                    );
                    let binding: UiBinding = serde_json::from_value(Value::Object(object))
                        .map_err(|e| op_error("ui.op.binding", e.to_string()))?;
                    match slot {
                        Some(slot) => root.bindings[slot] = binding,
                        None => root.bindings.push(binding),
                    }
                }
            }
            report.ids = vec![node.clone()];
        }
        UiOp::SetRoot { patch } => {
            check_patch(patch)?;
            if let Some(key) = patch
                .keys()
                .find(|k| !ROOT_PATCH_KEYS.contains(&k.as_str()))
            {
                return Err(op_error(
                    "ui.op.field",
                    format!(
                        "set_root cannot patch `{key}` (allowed: {})",
                        ROOT_PATCH_KEYS.join(", ")
                    ),
                ));
            }
            for (key, value) in patch {
                let bad = || op_error("ui.op.field", format!("`{key}` has the wrong type"));
                match (key.as_str(), value) {
                    ("layer", Value::Null) => root.layer = 0,
                    ("layer", v) => {
                        let n = v.as_i64().ok_or_else(bad)?;
                        root.layer = i32::try_from(n).map_err(|_| bad())?;
                    }
                    ("visible", Value::Null) => root.visible = true,
                    ("visible", v) => root.visible = v.as_bool().ok_or_else(bad)?,
                    ("enabled", Value::Null) => root.enabled = true,
                    ("enabled", v) => root.enabled = v.as_bool().ok_or_else(bad)?,
                    ("theme", Value::Null) => root.theme = None,
                    ("theme", v) => root.theme = Some(v.as_str().ok_or_else(bad)?.to_owned()),
                    ("reducedMotion", Value::Null) => root.reduced_motion = None,
                    ("reducedMotion", v) => {
                        root.reduced_motion = Some(v.as_bool().ok_or_else(bad)?)
                    }
                    _ => return Err(bad()),
                }
            }
        }
    }
    Ok(report)
}

fn node_mut<'a>(root: &'a mut UiRoot, id: &str) -> Result<&'a mut UiNode> {
    root.tree
        .nodes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| unknown(id))
}

fn duplicate(
    root: &mut UiRoot,
    id: &str,
    new_id: Option<&str>,
    parent: Option<&Option<String>>,
    index: Option<usize>,
    report: &mut UiOpReport,
) -> Result<()> {
    let nodes = &mut root.tree.nodes;
    let at = position(nodes, id)?;
    let members = subtree(nodes, at);
    if nodes.len() + members.len() > MAX_UI_NODES {
        return Err(op_error(
            "ui.op.limit",
            format!("A UI root holds at most {MAX_UI_NODES} nodes"),
        ));
    }
    let mut taken: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for &member in &members {
        let old = nodes[member].id.clone();
        let fresh = match new_id {
            Some(wanted) if member == at => {
                check_id(wanted)?;
                if taken.contains(wanted) {
                    return Err(op_error(
                        "ui.op.duplicate_id",
                        format!("A node with ID `{wanted}` exists"),
                    ));
                }
                wanted.to_owned()
            }
            _ => fresh_id(&old, &taken),
        };
        taken.insert(fresh.clone());
        map.insert(old, fresh);
    }
    let original_parent = nodes[at].parent.clone();
    let new_parent: Option<String> = match parent {
        None => original_parent.clone(),
        Some(None) => None,
        Some(Some(parent)) => {
            position(nodes, parent)?;
            Some(parent.clone())
        }
    };
    let index = index.or_else(|| {
        (parent.is_none()).then(|| {
            children_of(nodes, original_parent.as_deref())
                .iter()
                .position(|i| *i == at)
                .map_or(0, |p| p + 1)
        })
    });
    let remap = |value: &mut Option<String>| {
        if let Some(mapped) = value.as_ref().and_then(|v| map.get(v)) {
            *value = Some(mapped.clone());
        }
    };
    let mut block = Vec::with_capacity(members.len());
    for &member in &members {
        let mut copy = nodes[member].clone();
        copy.id = map[&copy.id].clone();
        if member == at {
            copy.parent = new_parent.clone();
        } else {
            remap(&mut copy.parent);
        }
        for target in nav_targets_mut(&mut copy) {
            remap(target);
        }
        if let UiWidget::VirtualList { template, .. } = &mut copy.widget
            && let Some(mapped) = map.get(template.as_str())
        {
            *template = mapped.clone();
        }
        block.push(copy);
    }
    // The copy of the original leads the block so `index` addresses it.
    if let Some(first) = block.iter().position(|n| n.id == map[id]) {
        block.swap(0, first);
    }
    insert_block(nodes, block, new_parent.as_deref(), index)?;
    let copies: Vec<UiBinding> = root
        .bindings
        .iter()
        .filter(|b| map.contains_key(&b.node))
        .map(|b| {
            let mut copy = b.clone();
            copy.node = map[&b.node].clone();
            copy
        })
        .collect();
    root.bindings.extend(copies);
    report.ids = vec![map[id].clone()];
    report.id_map = map;
    Ok(())
}

/// Apply one operation to a copy of `root` without validating the result.
pub(super) fn apply_one(root: &UiRoot, op: &UiOp) -> Result<UiRoot> {
    let mut work = root.clone();
    apply_op(&mut work, op)?;
    Ok(work)
}

/// Apply `ops` to a copy of `root`, validate the result and return it with one
/// report per operation. `root` is never modified; any failure returns the
/// error of the first failing operation (or of the final validation).
pub fn apply_ops(root: &UiRoot, ops: &[UiOp]) -> Result<(UiRoot, Vec<UiOpReport>)> {
    if ops.len() > MAX_UI_OPS {
        return Err(op_error(
            "ui.op.limit",
            format!("At most {MAX_UI_OPS} operations per batch"),
        ));
    }
    let mut work = root.clone();
    let mut reports = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        reports.push(apply_op(&mut work, op).map_err(|e| context(e, i, op.name()))?);
    }
    work.validate().map_err(|e| {
        let (code, message) = e
            .diagnostics
            .first()
            .map(|d| (d.code.clone(), d.message.clone()))
            .unwrap_or_default();
        Error::new(&code, format!("The edited document is invalid: {message}"))
    })?;
    Ok((work, reports))
}

impl UiTree {
    /// The same tree with nodes in depth-first order (top-level nodes in
    /// vector order, each followed by its subtree). Sibling order is
    /// preserved, so equal canonical trees describe the same hierarchy
    /// regardless of where unrelated subtrees sit in the vector.
    pub fn canonical(&self) -> UiTree {
        let mut children: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            children.entry(node.parent.as_deref()).or_default().push(i);
        }
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<usize> = children.get(&None).cloned().unwrap_or_default();
        stack.reverse();
        while let Some(i) = stack.pop() {
            order.push(i);
            if let Some(kids) = children.get(&Some(self.nodes[i].id.as_str())) {
                stack.extend(kids.iter().rev().copied());
            }
        }
        if order.len() != self.nodes.len() {
            return self.clone();
        }
        UiTree {
            nodes: order.into_iter().map(|i| self.nodes[i].clone()).collect(),
        }
    }
}

// ---- change notification ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiChangeKind {
    Edit,
    Undo,
    Redo,
    Theme,
    Selection,
}

/// One document change, delivered to listeners and queued for
/// [`UiDocument::take_changes`].
#[derive(Clone, Debug, PartialEq)]
pub struct UiChange {
    /// Document generation after the change.
    pub generation: u64,
    pub kind: UiChangeKind,
    pub label: String,
    pub diff: UiDiff,
    /// Selection after the change.
    pub selection: Vec<String>,
}

type Listener = Box<dyn Fn(&UiChange) + Send + Sync>;

struct Step {
    label: String,
    root: UiRoot,
}

/// An editable UI tree with history, selection and change notification.
pub struct UiDocument {
    root: UiRoot,
    theme: Option<UiTheme>,
    selection: Vec<String>,
    generation: u64,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    history_limit: usize,
    pending: VecDeque<UiChange>,
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
}

impl std::fmt::Debug for UiDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiDocument")
            .field("nodes", &self.root.tree.nodes.len())
            .field("generation", &self.generation)
            .field("undo", &self.undo.len())
            .field("redo", &self.redo.len())
            .field("selection", &self.selection)
            .finish()
    }
}

impl UiDocument {
    /// Wrap a validated root.
    pub fn new(root: UiRoot) -> Result<Self> {
        root.validate()?;
        Ok(Self {
            root,
            theme: None,
            selection: vec![],
            generation: 0,
            undo: VecDeque::new(),
            redo: vec![],
            history_limit: DEFAULT_UI_HISTORY,
            pending: VecDeque::new(),
            listeners: vec![],
            next_listener: 0,
        })
    }

    /// Parse the `data` of an `engine.UiRoot` component.
    pub fn from_json(data: &Value) -> Result<Self> {
        let root: UiRoot = serde_json::from_value(data.clone())
            .map_err(|e| op_error("ui.tree.invalid", e.to_string()))?;
        Self::new(root)
    }

    pub fn root(&self) -> &UiRoot {
        &self.root
    }

    pub fn into_root(self) -> UiRoot {
        self.root
    }

    /// The root as component JSON (omitting default values).
    pub fn to_json(&self) -> Value {
        self.root.to_json()
    }

    pub fn theme(&self) -> Option<&UiTheme> {
        self.theme.as_ref()
    }

    /// Attach (or clear) the theme previews of this document use. Themes are
    /// not part of the undo history.
    pub fn set_theme(&mut self, theme: Option<UiTheme>) -> Result<()> {
        if let Some(theme) = &theme {
            theme.validate()?;
        }
        if self.theme == theme {
            return Ok(());
        }
        self.theme = theme;
        self.generation += 1;
        self.notify(UiChange {
            generation: self.generation,
            kind: UiChangeKind::Theme,
            label: "theme".into(),
            diff: UiDiff::default(),
            selection: self.selection.clone(),
        });
        Ok(())
    }

    /// Increments on every edit, undo, redo and theme change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    // ---- edits ----

    /// Apply one operation as one undo step.
    pub fn apply(&mut self, op: UiOp) -> Result<UiOpReport> {
        let label = op.name().to_owned();
        let mut reports = self.apply_labeled(&label, vec![op])?;
        Ok(reports.remove(0))
    }

    /// Apply `ops` atomically as ONE undo step.
    pub fn apply_batch(&mut self, ops: Vec<UiOp>) -> Result<Vec<UiOpReport>> {
        let label = format!("batch of {}", ops.len());
        self.apply_labeled(&label, ops)
    }

    /// [`Self::apply_batch`] with a history label.
    pub fn apply_labeled(&mut self, label: &str, ops: Vec<UiOp>) -> Result<Vec<UiOpReport>> {
        let (next, reports) = apply_ops(&self.root, &ops)?;
        if next == self.root {
            return Ok(reports);
        }
        let diff = diff_roots(&self.root, &next);
        let previous = std::mem::replace(&mut self.root, next);
        self.push_history(label, previous);
        self.redo.clear();
        for report in &reports {
            for (old, new) in &report.id_map {
                if report.op == "rename_node" {
                    for selected in &mut self.selection {
                        if selected == old {
                            *selected = new.clone();
                        }
                    }
                }
            }
        }
        self.prune_selection();
        self.generation += 1;
        self.notify(UiChange {
            generation: self.generation,
            kind: UiChangeKind::Edit,
            label: label.to_owned(),
            diff,
            selection: self.selection.clone(),
        });
        Ok(reports)
    }

    /// Validate `ops` against the current state without changing anything.
    pub fn preview_ops(&self, ops: &[UiOp]) -> Result<(UiRoot, Vec<UiOpReport>)> {
        apply_ops(&self.root, ops)
    }

    fn push_history(&mut self, label: &str, root: UiRoot) {
        self.undo.push_back(Step {
            label: label.to_owned(),
            root,
        });
        while self.undo.len() > self.history_limit {
            self.undo.pop_front();
        }
    }

    // ---- history ----

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Label of the step [`Self::undo`] would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Bound the undo depth (1 to [`MAX_UI_HISTORY`]); older steps drop.
    pub fn set_history_limit(&mut self, limit: usize) {
        self.history_limit = limit.clamp(1, MAX_UI_HISTORY);
        while self.undo.len() > self.history_limit {
            self.undo.pop_front();
        }
        self.redo.truncate(self.history_limit);
    }

    /// Revert the last step; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop_back() else {
            return false;
        };
        let diff = diff_roots(&self.root, &step.root);
        let current = std::mem::replace(&mut self.root, step.root);
        self.redo.push(Step {
            label: step.label.clone(),
            root: current,
        });
        self.after_history(UiChangeKind::Undo, step.label, diff);
        true
    }

    /// Re-apply the last undone step; `false` when there is none.
    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else {
            return false;
        };
        let diff = diff_roots(&self.root, &step.root);
        let current = std::mem::replace(&mut self.root, step.root);
        self.undo.push_back(Step {
            label: step.label.clone(),
            root: current,
        });
        self.after_history(UiChangeKind::Redo, step.label, diff);
        true
    }

    fn after_history(&mut self, kind: UiChangeKind, label: String, diff: UiDiff) {
        self.prune_selection();
        self.generation += 1;
        self.notify(UiChange {
            generation: self.generation,
            kind,
            label,
            diff,
            selection: self.selection.clone(),
        });
    }

    // ---- selection ----

    pub fn selection(&self) -> &[String] {
        &self.selection
    }

    /// Replace the selection; every ID must name a node. Duplicates collapse.
    pub fn select(&mut self, ids: &[String]) -> Result<()> {
        let mut next: Vec<String> = Vec::with_capacity(ids.len());
        for id in ids {
            if self.root.tree.find(id).is_none() {
                return Err(unknown(id));
            }
            if !next.contains(id) {
                next.push(id.clone());
            }
        }
        if next != self.selection {
            self.selection = next;
            self.notify(UiChange {
                generation: self.generation,
                kind: UiChangeKind::Selection,
                label: "selection".into(),
                diff: UiDiff::default(),
                selection: self.selection.clone(),
            });
        }
        Ok(())
    }

    pub fn clear_selection(&mut self) {
        let _ = self.select(&[]);
    }

    fn prune_selection(&mut self) {
        let nodes = &self.root.tree.nodes;
        self.selection
            .retain(|id| nodes.iter().any(|n| n.id == *id));
    }

    // ---- notification ----

    /// Register a callback run after every change.
    pub fn add_listener(&mut self, listener: impl Fn(&UiChange) + Send + Sync + 'static) -> u64 {
        self.next_listener += 1;
        self.listeners
            .push((self.next_listener, Box::new(listener)));
        self.next_listener
    }

    pub fn remove_listener(&mut self, id: u64) -> bool {
        let before = self.listeners.len();
        self.listeners.retain(|(listener, _)| *listener != id);
        self.listeners.len() != before
    }

    /// Drain the queued change records (the newest 256 are kept).
    pub fn take_changes(&mut self) -> Vec<UiChange> {
        self.pending.drain(..).collect()
    }

    fn notify(&mut self, change: UiChange) {
        for (_, listener) in &self.listeners {
            listener(&change);
        }
        self.pending.push_back(change);
        while self.pending.len() > MAX_PENDING_CHANGES {
            self.pending.pop_front();
        }
    }

    /// What differs between the document and `other`.
    pub fn diff_from(&self, other: &UiRoot) -> UiDiff {
        diff_roots(other, &self.root)
    }
}
