//! Differences between two UI roots, as changed IDs ([`UiDiff`]) or as the
//! [`UiOp`] list that turns one into the other ([`diff_ops`]).
use super::{
    document::{apply_one, make_patch},
    *,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Fields of one surviving node that differ.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct UiNodeChange {
    pub id: String,
    /// Any of `parent`, `widget`, `style`, `visible`, `enabled`, `classes`,
    /// `tooltip`, `nav`, `accessibility`.
    pub fields: Vec<String>,
}

/// Structural and field-level difference of two roots.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiDiff {
    /// Root fields that differ: `layer`, `visible`, `enabled`, `theme`, `reducedMotion`.
    pub root: Vec<String>,
    /// The binding list differs.
    pub bindings: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<UiNodeChange>,
    /// Parents (`None`: top level) whose child order differs, counting only
    /// children that kept that parent.
    pub reordered: Vec<Option<String>>,
}

impl UiDiff {
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
            && !self.bindings
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.reordered.is_empty()
    }

    /// Every added, removed or field-changed node ID, sorted and unique.
    pub fn changed_ids(&self) -> Vec<String> {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        ids.extend(self.added.iter().map(String::as_str));
        ids.extend(self.removed.iter().map(String::as_str));
        ids.extend(self.changed.iter().map(|c| c.id.as_str()));
        ids.into_iter().map(str::to_owned).collect()
    }
}

fn index_by_id(root: &UiRoot) -> BTreeMap<&str, &UiNode> {
    root.tree.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
}

fn node_fields(a: &UiNode, b: &UiNode) -> Vec<String> {
    let mut fields = vec![];
    let mut note = |differs: bool, name: &str| {
        if differs {
            fields.push(name.to_owned());
        }
    };
    note(a.parent != b.parent, "parent");
    note(a.widget != b.widget, "widget");
    note(a.style != b.style, "style");
    note(a.visible != b.visible, "visible");
    note(a.enabled != b.enabled, "enabled");
    note(a.classes != b.classes, "classes");
    note(a.tooltip != b.tooltip, "tooltip");
    note(a.nav != b.nav, "nav");
    note(a.accessibility != b.accessibility, "accessibility");
    fields
}

/// Compare `a` (before) with `b` (after).
pub fn diff_roots(a: &UiRoot, b: &UiRoot) -> UiDiff {
    let (old, new) = (index_by_id(a), index_by_id(b));
    let mut diff = UiDiff::default();
    let mut note = |differs: bool, name: &str| {
        if differs {
            diff.root.push(name.to_owned());
        }
    };
    note(a.layer != b.layer, "layer");
    note(a.visible != b.visible, "visible");
    note(a.enabled != b.enabled, "enabled");
    note(a.theme != b.theme, "theme");
    note(a.reduced_motion != b.reduced_motion, "reducedMotion");
    diff.bindings = a.bindings != b.bindings;
    diff.removed = a
        .tree
        .nodes
        .iter()
        .filter(|n| !new.contains_key(n.id.as_str()))
        .map(|n| n.id.clone())
        .collect();
    for node in &b.tree.nodes {
        match old.get(node.id.as_str()) {
            None => diff.added.push(node.id.clone()),
            Some(previous) => {
                let fields = node_fields(previous, node);
                if !fields.is_empty() {
                    diff.changed.push(UiNodeChange {
                        id: node.id.clone(),
                        fields,
                    });
                }
            }
        }
    }
    let mut parents: BTreeSet<Option<&str>> = BTreeSet::new();
    parents.extend(a.tree.nodes.iter().map(|n| n.parent.as_deref()));
    parents.extend(b.tree.nodes.iter().map(|n| n.parent.as_deref()));
    parents.insert(None);
    for parent in parents {
        let kept = |root: &UiRoot, other: &BTreeMap<&str, &UiNode>| -> Vec<String> {
            root.tree
                .nodes
                .iter()
                .filter(|n| n.parent.as_deref() == parent)
                .filter(|n| {
                    other
                        .get(n.id.as_str())
                        .is_some_and(|o| o.parent.as_deref() == parent)
                })
                .map(|n| n.id.clone())
                .collect()
        };
        if kept(a, &new) != kept(b, &old) {
            diff.reordered.push(parent.map(str::to_owned));
        }
    }
    diff
}

fn json_of<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn depth_of(root: &UiRoot, id: &str) -> usize {
    let index = index_by_id(root);
    let mut depth = 0;
    let mut cursor = index.get(id).and_then(|n| n.parent.as_deref());
    while let Some(parent) = cursor {
        depth += 1;
        if depth > MAX_UI_DEPTH {
            break;
        }
        cursor = index.get(parent).and_then(|n| n.parent.as_deref());
    }
    depth
}

/// Index of `id` among its parent's children in `root`.
fn sibling_index(root: &UiRoot, id: &str) -> Option<usize> {
    let parent = root.tree.nodes.iter().find(|n| n.id == id)?.parent.clone();
    root.tree
        .nodes
        .iter()
        .filter(|n| n.parent == parent)
        .position(|n| n.id == id)
}

/// The operations that turn `a` into `b`. Applying them to `a` (as one batch)
/// yields a root whose [`UiTree::canonical`] tree equals that of `b`; the
/// position of unrelated subtrees inside the node vector is not reproduced.
/// Both roots must be valid.
pub fn diff_ops(a: &UiRoot, b: &UiRoot) -> Result<Vec<UiOp>> {
    let diff = diff_roots(a, b);
    let mut ops: Vec<UiOp> = vec![];
    let mut work = a.clone();
    if !diff.root.is_empty() {
        let mut patch = Map::new();
        for field in &diff.root {
            let value = match field.as_str() {
                "layer" => json!(b.layer),
                "visible" => json!(b.visible),
                "enabled" => json!(b.enabled),
                "theme" => json!(b.theme),
                _ => json!(b.reduced_motion),
            };
            patch.insert(field.clone(), value);
        }
        push(&mut ops, &mut work, UiOp::SetRoot { patch })?;
    }
    let removed: BTreeSet<&str> = diff.removed.iter().map(String::as_str).collect();
    let new = index_by_id(b);
    // Survivors that hang below a removed node move out first.
    for node in &a.tree.nodes {
        if new.contains_key(node.id.as_str())
            && node.parent.as_deref().is_some_and(|p| removed.contains(p))
        {
            push(
                &mut ops,
                &mut work,
                UiOp::MoveNode {
                    id: node.id.clone(),
                    parent: Some(None),
                    index: None,
                },
            )?;
        }
    }
    for id in &diff.removed {
        // A removed node's subtree can already be gone.
        if work.tree.find(id).is_some() {
            push(&mut ops, &mut work, UiOp::RemoveNode { id: id.clone() })?;
        }
    }
    let mut added: Vec<&String> = diff.added.iter().collect();
    added.sort_by_key(|id| depth_of(b, id));
    for id in added {
        let node = new[id.as_str()];
        let mut fields = json_of(node);
        let Some(object) = fields.as_object_mut() else {
            continue;
        };
        object.remove("id");
        object.remove("parent");
        let widget = object
            .remove("widget")
            .unwrap_or_else(|| json!("container"));
        push(
            &mut ops,
            &mut work,
            UiOp::AddNode {
                id: id.clone(),
                widget,
                parent: node.parent.clone(),
                index: None,
                node: Some(std::mem::take(object)),
            },
        )?;
    }
    // Reparent survivors in two passes so swapped hierarchies never cycle.
    let reparent: Vec<&UiNodeChange> = diff
        .changed
        .iter()
        .filter(|c| c.fields.iter().any(|f| f == "parent"))
        .collect();
    for change in &reparent {
        push(
            &mut ops,
            &mut work,
            UiOp::MoveNode {
                id: change.id.clone(),
                parent: Some(None),
                index: None,
            },
        )?;
    }
    let mut ordered = reparent;
    ordered.sort_by_key(|c| depth_of(b, &c.id));
    for change in ordered {
        if let Some(parent) = new[change.id.as_str()].parent.clone() {
            push(
                &mut ops,
                &mut work,
                UiOp::MoveNode {
                    id: change.id.clone(),
                    parent: Some(Some(parent)),
                    index: None,
                },
            )?;
        }
    }
    let old = index_by_id(a);
    for change in &diff.changed {
        let (before, after) = (old[change.id.as_str()], new[change.id.as_str()]);
        for field in &change.fields {
            let op = match field.as_str() {
                "widget" => {
                    let (previous, next) = (json_of(&before.widget), json_of(&after.widget));
                    let patch = if previous["kind"] == next["kind"] {
                        make_patch(&previous, &next)
                    } else {
                        next
                    };
                    UiOp::SetWidget {
                        id: change.id.clone(),
                        patch: patch.as_object().cloned().unwrap_or_default(),
                    }
                }
                "style" => UiOp::SetStyle {
                    id: change.id.clone(),
                    patch: make_patch(&json_of(&before.style), &json_of(&after.style))
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                },
                "visible" | "enabled" | "tooltip" | "nav" => {
                    let (previous, next) = (json_of(before), json_of(after));
                    let mut patch = Map::new();
                    for key in ["visible", "enabled", "tooltip", "nav"] {
                        if previous.get(key) != next.get(key) {
                            let value = match (previous.get(key), next.get(key)) {
                                (Some(p), Some(n)) if p.is_object() && n.is_object() => {
                                    make_patch(p, n)
                                }
                                (_, Some(n)) => n.clone(),
                                (_, None) => Value::Null,
                            };
                            patch.insert(key.to_owned(), value);
                        }
                    }
                    // `visible`/`enabled` fall back to true when null.
                    UiOp::SetNode {
                        id: change.id.clone(),
                        patch,
                    }
                }
                "classes" => UiOp::SetClasses {
                    id: change.id.clone(),
                    classes: after.classes.clone(),
                },
                "accessibility" => UiOp::SetAccessibility {
                    id: change.id.clone(),
                    accessibility: after.accessibility.as_ref().map(json_of),
                },
                _ => continue,
            };
            // The node-level fields share one op; skip repeats.
            if matches!(op, UiOp::SetNode { .. })
                && ops
                    .iter()
                    .any(|o| matches!(o, UiOp::SetNode { id, .. } if *id == change.id))
            {
                continue;
            }
            push(&mut ops, &mut work, op)?;
        }
    }
    if diff.bindings {
        push(
            &mut ops,
            &mut work,
            UiOp::SetBindings {
                bindings: b.bindings.clone(),
            },
        )?;
    }
    // Finally restore sibling order group by group.
    let mut parents: Vec<Option<String>> = vec![None];
    parents.extend(b.tree.nodes.iter().map(|n| Some(n.id.clone())));
    for parent in parents {
        let wanted: Vec<&str> = b
            .tree
            .nodes
            .iter()
            .filter(|n| n.parent == parent)
            .map(|n| n.id.as_str())
            .collect();
        for (index, id) in wanted.into_iter().enumerate() {
            if sibling_index(&work, id) != Some(index) {
                push(
                    &mut ops,
                    &mut work,
                    UiOp::MoveNode {
                        id: id.to_owned(),
                        parent: None,
                        index: Some(index),
                    },
                )?;
            }
        }
    }
    Ok(ops)
}

/// Apply one operation without the final validation: intermediate states of
/// a diff sequence may be invalid even though the end state is not.
fn push(ops: &mut Vec<UiOp>, work: &mut UiRoot, op: UiOp) -> Result<()> {
    *work = apply_one(work, &op)?;
    ops.push(op);
    Ok(())
}
