//! UI authoring: the document model (operations, undo/redo, selection,
//! notifications, diff), layout inspection and the headless preview
//! rasterizer (`docs/ui-authoring.md`).
use engine_project::{
    content::{FontAtlas, FontAtlasMode, FontFamilySettings, FontGlyph, Mesh, Texture, Vertex},
    runtime::{Draw, DrawSpace, DrawState},
    ui::Rect,
    ui_tree::*,
};
use engine_text::AtlasConfig;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn px(v: f32) -> Value {
    json!({"unit": "pixels", "value": v})
}

fn base_json() -> Value {
    json!({"version": 1, "tree": {"nodes": [
        {"id": "panel", "style": {"width": px(300.0), "height": px(200.0),
            "layout": "column", "padding": 0, "gap": 0}},
        {"id": "title", "parent": "panel", "widget": {"kind": "label", "text": "Title"},
            "style": {"width": px(100.0), "height": px(20.0)}},
        {"id": "row", "parent": "panel", "style": {"width": px(300.0), "height": px(40.0),
            "layout": "row", "padding": 0, "gap": 0}},
        {"id": "ok", "parent": "row", "widget": {"kind": "button", "text": "OK", "action": "ok"},
            "style": {"width": px(100.0), "height": px(40.0)}, "nav": {"right": "cancel"}},
        {"id": "cancel", "parent": "row", "widget": {"kind": "button", "text": "Cancel"},
            "style": {"width": px(100.0), "height": px(40.0)}, "nav": {"left": "ok"}},
    ]}, "bindings": [{"node": "title", "property": "text", "path": "hud.title"}]})
}

fn base() -> UiDocument {
    UiDocument::from_json(&base_json()).unwrap()
}

fn ops(value: Value) -> Vec<UiOp> {
    UiOp::parse_all(&value).unwrap()
}

fn text_of(doc: &UiDocument) -> String {
    serde_json::to_string(doc.root()).unwrap()
}

fn ids(root: &UiRoot) -> Vec<&str> {
    root.tree.nodes.iter().map(|n| n.id.as_str()).collect()
}

fn node<'a>(root: &'a UiRoot, id: &str) -> &'a UiNode {
    &root.tree.nodes[root.tree.find(id).unwrap()]
}

// ---- operations and history ----

/// One representative operation per kind of the vocabulary.
fn one_of_each() -> Vec<Value> {
    vec![
        json!({"op": "add_node", "id": "hint", "widget": {"kind": "label", "text": "Hi"},
            "parent": "panel", "index": 1, "node": {"classes": ["note"], "visible": true}}),
        json!({"op": "remove_node", "id": "row"}),
        json!({"op": "move_node", "id": "cancel", "parent": "panel", "index": 0}),
        json!({"op": "move_node", "id": "ok", "index": 1}),
        json!({"op": "duplicate_node", "id": "row"}),
        json!({"op": "rename_node", "id": "ok", "new_id": "accept"}),
        json!({"op": "set_widget", "id": "ok", "patch": {"text": "Accept", "action": null}}),
        json!({"op": "set_widget", "id": "ok", "patch": {"kind": "toggle", "text": "On", "checked": true}}),
        json!({"op": "set_style", "id": "title", "patch": {"width": {"unit": "percent", "value": 0.5},
            "margin": {"left": 4}, "background": [1, 0, 0, 1]}}),
        json!({"op": "set_node", "id": "ok", "patch": {"enabled": false, "tooltip": {"text": "Go"},
            "nav": {"right": null, "tabIndex": 2}}}),
        json!({"op": "set_classes", "id": "ok", "classes": ["primary", "big"]}),
        json!({"op": "set_accessibility", "id": "ok", "accessibility": {"label": "Confirm", "role": "link"}}),
        json!({"op": "set_bindings", "bindings": []}),
        json!({"op": "set_binding", "node": "ok", "property": "text", "binding": {"path": "hud.ok"}}),
        json!({"op": "set_root", "patch": {"layer": 3, "theme": "dark", "reducedMotion": true}}),
    ]
}

#[test]
fn every_operation_applies_and_undo_redo_restores_identical_json() {
    for value in one_of_each() {
        let mut doc = base();
        let before = text_of(&doc);
        let op = ops(json!([value.clone()])).remove(0);
        assert_eq!(serde_json::to_value(&op).unwrap()["op"], value["op"]);
        doc.apply(op).unwrap_or_else(|e| panic!("{value}: {e}"));
        let after = text_of(&doc);
        assert_ne!(before, after, "{value} changed nothing");
        assert_eq!(doc.undo_depth(), 1, "{value}");

        assert!(doc.undo());
        assert_eq!(text_of(&doc), before, "undo of {value}");
        assert!(doc.redo());
        assert_eq!(text_of(&doc), after, "redo of {value}");
        assert!(doc.undo());
        assert_eq!(text_of(&doc), before, "second undo of {value}");
    }
}

#[test]
fn operations_round_trip_through_json() {
    let list = ops(json!(one_of_each()));
    let again = ops(serde_json::to_value(&list).unwrap());
    assert_eq!(list, again);
    assert!(UiOp::parse_all(&json!([{"op": "explode"}])).is_err());
    assert!(UiOp::parse_all(&json!([{"op": "remove_node", "id": "a", "extra": 1}])).is_err());
    assert!(UiOp::parse_all(&json!({"op": "remove_node"})).is_err());
    let many = vec![json!({"op": "remove_node", "id": "a"}); MAX_UI_OPS + 1];
    assert!(UiOp::parse_all(&json!(many)).is_err());
}

#[test]
fn a_batch_is_one_undo_step() {
    let mut doc = base();
    let before = text_of(&doc);
    let reports = doc
        .apply_batch(ops(json!([
            {"op": "add_node", "id": "a", "widget": "label", "parent": "panel"},
            {"op": "add_node", "id": "b", "widget": "button", "parent": "panel"},
            {"op": "set_style", "id": "a", "patch": {"opacity": 0.5}},
            {"op": "rename_node", "id": "b", "new_id": "c"},
        ])))
        .unwrap();
    assert_eq!(reports.len(), 4);
    assert_eq!(doc.undo_depth(), 1);
    assert_eq!(doc.generation(), 1);
    assert!(node(doc.root(), "c").widget.kind_name() == "button");
    assert!(doc.undo());
    assert!(!doc.can_undo());
    assert_eq!(text_of(&doc), before);
    assert!(doc.redo());
    assert!(doc.root().tree.find("c").is_some());
}

#[test]
fn rename_updates_every_reference() {
    let mut doc = UiDocument::from_json(&json!({"version": 1, "tree": {"nodes": [
        {"id": "list", "widget": {"kind": "virtual_list", "itemCount": 10, "itemExtent": 20,
            "template": "row"}},
        {"id": "row", "parent": "list", "widget": {"kind": "button", "text": "x"},
            "nav": {"down": "after"}},
        {"id": "after", "widget": {"kind": "button", "text": "y"}, "nav": {"up": "row"}},
        {"id": "kid", "parent": "row", "widget": {"kind": "label", "text": "k"}},
    ]}, "bindings": [{"node": "row", "property": "text", "path": "a.b"}]}))
    .unwrap();
    doc.select(&["row".to_owned(), "after".to_owned()]).unwrap();
    let report = doc
        .apply(UiOp::RenameNode {
            id: "row".into(),
            new_id: "item".into(),
        })
        .unwrap();
    assert_eq!(report.id_map["row"], "item");
    let root = doc.root();
    assert!(root.tree.find("row").is_none());
    assert_eq!(node(root, "kid").parent.as_deref(), Some("item"));
    assert_eq!(node(root, "after").nav.up.as_deref(), Some("item"));
    assert!(matches!(&node(root, "list").widget,
        UiWidget::VirtualList { template, .. } if template == "item"));
    assert_eq!(root.bindings[0].node, "item");
    assert_eq!(doc.selection(), ["item", "after"]);
    root.validate().unwrap();
    // Names that exist or are invalid are rejected.
    for new_id in ["after", ""] {
        let before = text_of(&doc);
        assert!(
            doc.apply(UiOp::RenameNode {
                id: "item".into(),
                new_id: new_id.into()
            })
            .is_err()
        );
        assert_eq!(text_of(&doc), before);
    }
}

#[test]
fn reparenting_rejects_cycles_and_leaves_the_document_unchanged() {
    let mut doc = base();
    let before = text_of(&doc);
    for (id, parent) in [
        ("panel", "ok"),
        ("panel", "row"),
        ("row", "row"),
        ("row", "cancel"),
    ] {
        let error = doc
            .apply(UiOp::MoveNode {
                id: id.into(),
                parent: Some(Some(parent.into())),
                index: None,
            })
            .unwrap_err();
        assert_eq!(
            error.diagnostics[0].code, "ui.op.cycle",
            "{id} into {parent}"
        );
    }
    assert_eq!(text_of(&doc), before);
    assert_eq!(doc.undo_depth(), 0);
    assert_eq!(doc.generation(), 0);
    // A legal move carries the subtree and respects the sibling index.
    doc.apply(UiOp::MoveNode {
        id: "row".into(),
        parent: Some(None),
        index: Some(0),
    })
    .unwrap();
    assert_eq!(node(doc.root(), "row").parent, None);
    assert_eq!(node(doc.root(), "ok").parent.as_deref(), Some("row"));
    assert!(doc.undo());
    assert_eq!(text_of(&doc), before);
}

#[test]
fn move_places_the_node_at_the_requested_sibling_index() {
    let mut doc = base();
    doc.apply(UiOp::MoveNode {
        id: "cancel".into(),
        parent: None,
        index: Some(0),
    })
    .unwrap();
    let siblings = |doc: &UiDocument, parent: &str| -> Vec<String> {
        doc.root()
            .tree
            .nodes
            .iter()
            .filter(|n| n.parent.as_deref() == Some(parent))
            .map(|n| n.id.clone())
            .collect()
    };
    assert_eq!(siblings(&doc, "row"), ["cancel", "ok"]);
    doc.apply(UiOp::MoveNode {
        id: "title".into(),
        parent: Some(Some("row".into())),
        index: Some(1),
    })
    .unwrap();
    assert_eq!(siblings(&doc, "row"), ["cancel", "title", "ok"]);
    assert_eq!(siblings(&doc, "panel"), ["row"]);
    let error = doc
        .apply(UiOp::MoveNode {
            id: "ok".into(),
            parent: None,
            index: Some(9),
        })
        .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "ui.op.index");
}

#[test]
fn duplicates_get_unique_ids_and_remap_internal_references() {
    let mut doc = base();
    let first = doc
        .apply(UiOp::DuplicateNode {
            id: "row".into(),
            new_id: None,
            parent: None,
            index: None,
        })
        .unwrap();
    assert_eq!(first.id_map["row"], "row_copy");
    assert_eq!(first.id_map["ok"], "ok_copy");
    let second = doc
        .apply(UiOp::DuplicateNode {
            id: "row".into(),
            new_id: None,
            parent: None,
            index: None,
        })
        .unwrap();
    assert_eq!(second.id_map["row"], "row_copy2");
    // Copying a copy never reuses an ID either.
    let third = doc
        .apply(UiOp::DuplicateNode {
            id: "row_copy".into(),
            new_id: Some("mine".into()),
            parent: Some(Some("panel".into())),
            index: Some(0),
        })
        .unwrap();
    assert_eq!(third.id_map["row_copy"], "mine");
    let all = ids(doc.root());
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(all.len(), unique.len(), "{all:?}");
    // Navigation inside the copied subtree points at the copy, not the original.
    assert_eq!(
        node(doc.root(), "ok_copy").nav.right.as_deref(),
        Some("cancel_copy")
    );
    assert_eq!(
        node(doc.root(), "cancel_copy2").nav.left.as_deref(),
        Some("ok_copy2")
    );
    // The copy follows its original among the siblings.
    let panel_children: Vec<&str> = doc
        .root()
        .tree
        .nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some("panel"))
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        panel_children,
        ["mine", "title", "row", "row_copy2", "row_copy"]
    );
    doc.root().validate().unwrap();
    // An explicit ID that exists is rejected.
    assert!(
        doc.apply(UiOp::DuplicateNode {
            id: "title".into(),
            new_id: Some("ok".into()),
            parent: None,
            index: None,
        })
        .is_err()
    );
}

#[test]
fn duplicating_a_node_with_bindings_copies_the_bindings() {
    let mut doc = base();
    doc.apply(UiOp::DuplicateNode {
        id: "title".into(),
        new_id: None,
        parent: None,
        index: None,
    })
    .unwrap();
    assert_eq!(doc.root().bindings.len(), 2);
    assert_eq!(doc.root().bindings[1].node, "title_copy");
    doc.root().validate().unwrap();
}

#[test]
fn invalid_operations_leave_the_document_unchanged() {
    let mut doc = base();
    let before = text_of(&doc);
    let bad: Vec<Value> = vec![
        json!({"op": "remove_node", "id": "ghost"}),
        json!({"op": "add_node", "id": "ok", "widget": "label"}),
        json!({"op": "add_node", "id": "x", "widget": "frobnicator"}),
        json!({"op": "add_node", "id": "x", "widget": "label", "parent": "ghost"}),
        json!({"op": "add_node", "id": "x", "widget": "image"}),
        json!({"op": "set_style", "id": "ok", "patch": {"opacity": 4}}),
        json!({"op": "set_style", "id": "ok", "patch": {"nonsense": 1}}),
        json!({"op": "set_widget", "id": "ok", "patch": {"kind": "slider", "min": 5, "max": 1}}),
        json!({"op": "set_node", "id": "ok", "patch": {"id": "z"}}),
        json!({"op": "set_classes", "id": "ok", "classes": ["bad class"]}),
        json!({"op": "set_binding", "node": "ok", "property": "value", "binding": {"path": "a"}}),
        json!({"op": "set_root", "patch": {"tree": {}}}),
        json!({"op": "move_node", "id": "ok"}),
    ];
    for value in bad {
        let op = ops(json!([value.clone()])).remove(0);
        let error = doc.apply(op).expect_err(&value.to_string());
        assert!(
            error.diagnostics[0].code.starts_with("ui."),
            "{value}: {error}"
        );
        assert_eq!(text_of(&doc), before, "{value}");
        assert_eq!(doc.undo_depth(), 0, "{value}");
        assert_eq!(doc.generation(), 0, "{value}");
    }
    // A later failure rolls back earlier operations of the batch.
    let error = doc
        .apply_batch(ops(json!([
            {"op": "set_style", "id": "ok", "patch": {"opacity": 0.5}},
            {"op": "remove_node", "id": "ghost"},
        ])))
        .unwrap_err();
    assert!(error.to_string().contains("op 1 (remove_node)"), "{error}");
    assert_eq!(text_of(&doc), before);
    // An edit that is individually fine but leaves the tree invalid is refused.
    let error = doc
        .apply_batch(ops(json!([
            {"op": "set_widget", "id": "ok",
                "patch": {"kind": "slider", "value": 5, "min": 0, "max": 1, "step": 0.1}}
        ])))
        .unwrap_err();
    assert!(error.to_string().contains("invalid"), "{error}");
    assert_eq!(text_of(&doc), before);
    // Node count is bounded while operations apply.
    let mut huge = vec![];
    for i in 0..MAX_UI_NODES - 4 {
        huge.push(json!({"op": "add_node", "id": format!("n{i}"), "widget": "container"}));
    }
    assert!(doc.apply_batch(ops(json!(huge))).is_err());
    assert_eq!(text_of(&doc), before);
}

#[test]
fn a_virtual_list_and_its_template_are_added_in_one_batch() {
    let mut doc = base();
    // The list is only valid once its template child exists.
    assert!(
        doc.apply(UiOp::AddNode {
            id: "items".into(),
            widget: json!({"kind": "virtual_list", "itemCount": 50, "itemExtent": 24,
                "template": "item"}),
            parent: Some("panel".into()),
            index: None,
            node: None,
        })
        .is_err()
    );
    doc.apply_batch(ops(json!([
        {"op": "add_node", "id": "items", "parent": "panel",
            "widget": {"kind": "virtual_list", "itemCount": 50, "itemExtent": 24, "template": "item"}},
        {"op": "add_node", "id": "item", "parent": "items", "widget": "button"},
    ])))
    .unwrap();
    assert_eq!(doc.undo_depth(), 1);
    // Renaming the template keeps the list valid; removing it alone does not.
    doc.apply(UiOp::RenameNode {
        id: "item".into(),
        new_id: "entry".into(),
    })
    .unwrap();
    assert!(doc.apply(UiOp::RemoveNode { id: "entry".into() }).is_err());
    doc.apply(UiOp::RemoveNode { id: "items".into() }).unwrap();
    assert!(doc.root().tree.find("entry").is_none());
}

#[test]
fn no_op_edits_do_not_create_history() {
    let mut doc = base();
    doc.apply(UiOp::SetClasses {
        id: "ok".into(),
        classes: vec![],
    })
    .unwrap();
    assert_eq!(doc.undo_depth(), 0);
    assert_eq!(doc.generation(), 0);
}

#[test]
fn patches_merge_nested_objects_and_replace_tagged_values() {
    let mut doc = base();
    doc.apply(UiOp::SetStyle {
        id: "ok".into(),
        patch: json!({"margin": {"left": 4}, "inset": {"top": 2}})
            .as_object()
            .unwrap()
            .clone(),
    })
    .unwrap();
    doc.apply(UiOp::SetStyle {
        id: "ok".into(),
        patch: json!({"margin": {"top": 6}, "inset": {"top": null}})
            .as_object()
            .unwrap()
            .clone(),
    })
    .unwrap();
    let style = &node(doc.root(), "ok").style;
    assert_eq!((style.margin.left, style.margin.top), (4.0, 6.0));
    assert_eq!(style.inset.top, None);
    // `{"unit":"fill"}` must not keep the old `value` around.
    doc.apply(UiOp::SetStyle {
        id: "ok".into(),
        patch: json!({"width": {"unit": "fill"}})
            .as_object()
            .unwrap()
            .clone(),
    })
    .unwrap();
    assert_eq!(node(doc.root(), "ok").style.width, UiLength::Fill);
    // `null` resets to the default.
    doc.apply(UiOp::SetStyle {
        id: "ok".into(),
        patch: json!({"width": null}).as_object().unwrap().clone(),
    })
    .unwrap();
    assert_eq!(node(doc.root(), "ok").style.width, UiStyle::DEFAULT.width);
}

#[test]
fn history_is_bounded_and_new_edits_clear_redo() {
    let mut doc = base();
    doc.set_history_limit(3);
    for i in 0..6 {
        doc.apply(UiOp::SetStyle {
            id: "ok".into(),
            patch: json!({"opacity": 0.1 * (i + 1) as f64})
                .as_object()
                .unwrap()
                .clone(),
        })
        .unwrap();
    }
    assert_eq!(doc.undo_depth(), 3);
    assert!(doc.undo() && doc.undo() && doc.undo());
    assert!(!doc.undo());
    assert!((node(doc.root(), "ok").style.opacity - 0.3).abs() < 1e-6);
    assert_eq!(doc.redo_depth(), 3);
    doc.apply(UiOp::SetClasses {
        id: "ok".into(),
        classes: vec!["a".into()],
    })
    .unwrap();
    assert!(!doc.can_redo());
    assert_eq!(doc.undo_label(), Some("set_classes"));
    doc.set_history_limit(5000);
    assert_eq!(doc.history_limit(), MAX_UI_HISTORY);
}

#[test]
fn selection_generation_and_change_notifications() {
    let mut doc = base();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let listener = doc.add_listener(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    doc.select(&["ok".to_owned(), "ok".to_owned(), "title".to_owned()])
        .unwrap();
    assert_eq!(doc.selection(), ["ok", "title"]);
    assert!(doc.select(&["ghost".to_owned()]).is_err());
    assert_eq!(
        doc.selection(),
        ["ok", "title"],
        "a failed select changes nothing"
    );
    assert_eq!(doc.generation(), 0, "selection is not a content change");

    doc.apply(UiOp::RemoveNode { id: "ok".into() }).unwrap();
    assert_eq!(
        doc.selection(),
        ["title"],
        "removed nodes leave the selection"
    );
    assert_eq!(doc.generation(), 1);
    assert!(doc.undo());
    assert!(doc.redo());
    assert_eq!(doc.generation(), 3);
    assert_eq!(seen.load(Ordering::SeqCst), 4);

    let changes = doc.take_changes();
    let kinds: Vec<_> = changes.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            UiChangeKind::Selection,
            UiChangeKind::Edit,
            UiChangeKind::Undo,
            UiChangeKind::Redo
        ]
    );
    assert_eq!(changes[1].diff.removed, ["ok"]);
    assert_eq!(changes[2].diff.added, ["ok"]);
    assert_eq!(changes[3].generation, 3);
    assert!(doc.take_changes().is_empty());
    assert!(doc.remove_listener(listener));
    assert!(!doc.remove_listener(listener));

    let mut themed = base();
    themed.set_theme(Some(UiTheme::new("dark"))).unwrap();
    assert_eq!(themed.generation(), 1);
    assert!(themed.set_theme(Some(UiTheme::new("bad name!"))).is_err());
}

// ---- diff ----

fn canonical_equal(a: &UiRoot, b: &UiRoot) -> bool {
    a.tree.canonical() == b.tree.canonical()
        && a.bindings == b.bindings
        && (a.layer, a.visible, a.enabled, &a.theme, a.reduced_motion)
            == (b.layer, b.visible, b.enabled, &b.theme, b.reduced_motion)
}

#[test]
fn diff_reports_changed_ids() {
    let a = base();
    let mut b = base();
    b.apply_batch(ops(json!([
        {"op": "set_style", "id": "ok", "patch": {"opacity": 0.5}},
        {"op": "remove_node", "id": "cancel"},
        {"op": "add_node", "id": "extra", "widget": "label", "parent": "panel"},
        {"op": "move_node", "id": "title", "index": 1},
        {"op": "set_root", "patch": {"layer": 2}},
    ])))
    .unwrap();
    let diff = diff_roots(a.root(), b.root());
    assert_eq!(diff.added, ["extra"]);
    assert_eq!(diff.removed, ["cancel"]);
    assert_eq!(diff.root, ["layer"]);
    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].id, "ok");
    // Removing `cancel` also cleared the navigation override that pointed at it.
    assert_eq!(diff.changed[0].fields, ["style", "nav"]);
    assert_eq!(diff.reordered, [Some("panel".to_owned())]);
    assert_eq!(diff.changed_ids(), ["cancel", "extra", "ok"]);
    assert!(diff_roots(a.root(), a.root()).is_empty());
    assert_eq!(a.diff_from(b.root()), diff_roots(b.root(), a.root()));
}

#[test]
fn diff_ops_round_trip_to_an_equivalent_tree() {
    let a = base();
    let targets = [
        // A hierarchy with reparenting, a swapped parent/child pair, a new
        // widget kind, reordering and a removed subtree.
        json!({"version": 1, "layer": 4, "theme": "dark", "tree": {"nodes": [
            {"id": "ok", "widget": {"kind": "toggle", "text": "OK", "checked": true},
                "style": {"width": px(50.0), "height": px(10.0)}, "classes": ["a"]},
            {"id": "panel", "parent": "ok", "style": {"width": px(300.0), "height": px(200.0)}},
            {"id": "cancel", "parent": "panel", "widget": {"kind": "button", "text": "Cancel"},
                "tooltip": {"text": "x"}, "nav": {"up": "ok", "tabIndex": 3}},
            {"id": "title", "parent": "panel", "widget": {"kind": "label", "text": "T2"},
                "accessibility": {"label": "Title"}, "visible": false},
            {"id": "fresh", "widget": {"kind": "slider", "value": 0.5, "min": 0, "max": 1, "step": 0.1},
                "enabled": false},
        ]}, "bindings": []}),
        // Nothing changes.
        base_json(),
        // Everything goes.
        json!({"version": 1, "tree": {"nodes": []}}),
        // Reordering only.
        json!({"version": 1, "tree": {"nodes": [
            {"id": "panel", "style": {"width": px(300.0), "height": px(200.0),
                "layout": "column", "padding": 0, "gap": 0}},
            {"id": "row", "parent": "panel", "style": {"width": px(300.0), "height": px(40.0),
                "layout": "row", "padding": 0, "gap": 0}},
            {"id": "cancel", "parent": "row", "widget": {"kind": "button", "text": "Cancel"},
                "style": {"width": px(100.0), "height": px(40.0)}, "nav": {"left": "ok"}},
            {"id": "ok", "parent": "row", "widget": {"kind": "button", "text": "OK", "action": "ok"},
                "style": {"width": px(100.0), "height": px(40.0)}, "nav": {"right": "cancel"}},
            {"id": "title", "parent": "panel", "widget": {"kind": "label", "text": "Title"},
                "style": {"width": px(100.0), "height": px(20.0)}},
        ]}, "bindings": [{"node": "title", "property": "text", "path": "hud.title"}]}),
    ];
    for (n, target) in targets.iter().enumerate() {
        let b = UiDocument::from_json(target).unwrap();
        let list = diff_ops(a.root(), b.root()).unwrap_or_else(|e| panic!("target {n}: {e}"));
        let (c, _) = apply_ops(a.root(), &list).unwrap_or_else(|e| panic!("target {n}: {e}"));
        assert!(
            canonical_equal(&c, b.root()),
            "target {n}\nops: {}\ngot  {}\nwant {}",
            serde_json::to_string(&list).unwrap(),
            serde_json::to_string(&c.tree.canonical()).unwrap(),
            serde_json::to_string(&b.root().tree.canonical()).unwrap()
        );
        if n == 1 {
            assert!(list.is_empty());
        }
    }
}

// ---- inspection ----

struct Fixture {
    fonts: BTreeMap<String, FontAtlas>,
    textures: BTreeMap<String, Arc<Texture>>,
}

fn fixture() -> Fixture {
    Fixture {
        fonts: BTreeMap::from([(
            "font".into(),
            FontAtlas {
                texture: "atlas".into(),
                line_height: 8.0,
                fallback: 65,
                glyphs: BTreeMap::from([(
                    65,
                    FontGlyph {
                        source: [0, 0, 4, 8],
                        advance: 5.0,
                        bearing: [0.0; 2],
                    },
                )]),
            },
        )]),
        textures: BTreeMap::from([(
            "atlas".into(),
            Arc::new(Texture {
                width: 4,
                height: 8,
                pixels: vec![255; 128],
            }),
        )]),
    }
}

fn rect_close(a: Rect, b: Rect) -> bool {
    [
        (a.x, b.x),
        (a.y, b.y),
        (a.width, b.width),
        (a.height, b.height),
    ]
    .iter()
    .all(|(x, y)| (x - y).abs() < 1e-4)
}

#[test]
fn inspection_boxes_match_layout_with() {
    let fixture = fixture();
    let backend = BitmapTextBackend::new(&fixture.fonts, &fixture.textures);
    let mut doc = base();
    // A content-sized label so the text backend matters.
    doc.apply_batch(ops(json!([
        {"op": "set_style", "id": "title", "patch": {"width": {"unit": "auto"}, "font": "font",
            "alignSelf": "start"}},
        {"op": "set_widget", "id": "title", "patch": {"text": "AAA"}},
    ])))
    .unwrap();
    let viewport = [640, 480];
    let boxes = doc.root().tree.layout_with(viewport, &backend).unwrap();
    let inspection = doc.inspect(viewport, &backend, &BTreeMap::new()).unwrap();
    assert_eq!(inspection.nodes.len(), boxes.len());
    for (laid, seen) in boxes.iter().zip(&inspection.nodes) {
        assert_eq!(seen.id, doc.root().tree.nodes[laid.node].id);
        assert!(rect_close(seen.rect, laid.rect), "{}", seen.id);
        assert!(rect_close(seen.content, laid.content), "{}", seen.id);
        assert!(rect_close(seen.clip, laid.clip), "{}", seen.id);
        assert!(rect_close(seen.visible_rect, laid.visible()), "{}", seen.id);
        assert_eq!(seen.enabled, laid.enabled);
    }
    let title = inspection.node("title").unwrap();
    assert_eq!(
        title.rect.width,
        3.0 * 5.0 + 8.0,
        "text 15 plus default padding 4+4"
    );
    assert_eq!(title.kind, "label");
    assert_eq!(title.parent.as_deref(), Some("panel"));
    assert_eq!(title.depth, 1);
    assert!(inspection.unplaced.is_empty());
    let value = title.to_json();
    assert_eq!(value["rect"], json!([0.0, 0.0, 23.0, 20.0]));
}

#[test]
fn hit_testing_returns_the_topmost_node() {
    let doc = base();
    let inspection = doc
        .inspect([640, 480], &NullTextBackend, &BTreeMap::new())
        .unwrap();
    let root = doc.root();
    let at = |p: [f32; 2], mode| inspection.hit_test(p, mode, root).map(|n| n.id.as_str());
    // panel column: title 0..20, row 20..60 with ok 0..100 and cancel 100..200.
    assert_eq!(at([10.0, 30.0], UiHitMode::Any), Some("ok"));
    assert_eq!(at([150.0, 30.0], UiHitMode::Any), Some("cancel"));
    assert_eq!(at([250.0, 30.0], UiHitMode::Any), Some("row"));
    assert_eq!(at([250.0, 100.0], UiHitMode::Any), Some("panel"));
    assert_eq!(at([10.0, 10.0], UiHitMode::Any), Some("title"));
    assert_eq!(at([500.0, 400.0], UiHitMode::Any), None);
    assert_eq!(at([10.0, 30.0], UiHitMode::Interactive), Some("ok"));
    assert_eq!(at([250.0, 100.0], UiHitMode::Interactive), None);
    let stack: Vec<&str> = inspection
        .hit_stack([10.0, 30.0], UiHitMode::Any, root)
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(stack, ["ok", "row", "panel"]);
}

#[test]
fn inspection_reports_layout_issues() {
    let doc = UiDocument::from_json(&json!({"version": 1, "tree": {"nodes": [
        {"id": "panel", "style": {"width": px(100.0), "height": px(50.0), "clip": true, "padding": 0}},
        {"id": "wide", "parent": "panel", "widget": {"kind": "button", "text": "w"},
            "style": {"width": px(180.0), "height": px(20.0)}},
        {"id": "flat", "parent": "panel", "style": {"width": px(0.0), "height": px(10.0)}},
        {"id": "away", "parent": "panel", "widget": {"kind": "button", "text": "a"},
            "style": {"width": px(20.0), "height": px(20.0), "offset": [400.0, 0.0]}},
        {"id": "gone", "style": {"width": px(20.0), "height": px(20.0), "offset": [2000.0, 0.0]}},
        {"id": "off", "widget": {"kind": "button", "text": "off"}, "enabled": true,
            "style": {"width": px(20.0), "height": px(20.0), "offset": [0.0, 100.0]}},
        {"id": "locked", "enabled": false, "style": {"width": px(50.0), "height": px(20.0),
            "offset": [0.0, 150.0]}},
        {"id": "inner", "parent": "locked", "widget": {"kind": "button", "text": "i"},
            "style": {"width": px(20.0), "height": px(20.0)}},
        {"id": "jump", "widget": {"kind": "button", "text": "j"}, "nav": {"right": "title"},
            "style": {"width": px(20.0), "height": px(20.0), "offset": [0.0, 200.0]}},
        {"id": "title", "widget": {"kind": "label", "text": "t"},
            "style": {"width": px(20.0), "height": px(20.0), "offset": [0.0, 250.0]}},
    ]}}))
    .unwrap();
    let inspection = doc
        .inspect([800, 600], &NullTextBackend, &BTreeMap::new())
        .unwrap();
    let has = |code: &str, node: &str| {
        inspection
            .issues
            .iter()
            .any(|i| i.code == format!("ui.issue.{code}") && i.node == node)
    };
    assert!(has("overflow", "wide"), "{:#?}", inspection.issues);
    assert!(has("zero_size", "flat"));
    assert!(has("clipped_away", "away"));
    assert!(has("clipped_away", "gone"), "outside the viewport");
    assert!(has("unreachable_focus", "inner"), "disabled ancestor");
    assert!(has("nav_target_not_focusable", "jump"));
    assert!(!has("overflow", "off"));
    assert!(!has("unreachable_focus", "off"));
    assert!(inspection.issues.iter().all(|i| !i.message.is_empty()));
    assert!(!inspection.issues_truncated);

    // An open dialog traps focus: buttons beneath it are unreachable.
    let modal = UiDocument::from_json(&json!({"version": 1, "tree": {"nodes": [
        {"id": "under", "widget": {"kind": "button", "text": "u"},
            "style": {"width": px(20.0), "height": px(20.0)}},
        {"id": "dialog", "widget": {"kind": "modal_dialog", "title": "T", "open": true,
            "buttons": ["Ok"]}, "style": {"width": px(200.0), "height": px(100.0)}},
        {"id": "inside", "parent": "dialog", "widget": {"kind": "button", "text": "i"},
            "style": {"width": px(20.0), "height": px(20.0)}},
    ]}}))
    .unwrap();
    let inspection = modal
        .inspect([800, 600], &NullTextBackend, &BTreeMap::new())
        .unwrap();
    let issue = |node: &str| {
        inspection
            .issues
            .iter()
            .any(|i| i.code == "ui.issue.unreachable_focus" && i.node == node)
    };
    assert!(issue("under"), "{:#?}", inspection.issues);
    assert!(!issue("inside"));
    assert!(inspection.nodes.iter().any(|n| n.overlay));
}

#[test]
fn inspection_reports_unplaced_nodes_and_state_flags() {
    let mut doc = base();
    doc.apply(UiOp::SetNode {
        id: "cancel".into(),
        patch: json!({"visible": false}).as_object().unwrap().clone(),
    })
    .unwrap();
    let mut states = BTreeMap::new();
    let mut flags = UiStateFlags::default();
    flags.insert(UiStateFlags::HOVERED);
    flags.insert(UiStateFlags::FOCUSED);
    states.insert("ok".to_owned(), flags);
    let inspection = doc.inspect([640, 480], &NullTextBackend, &states).unwrap();
    assert_eq!(inspection.unplaced, ["cancel"]);
    assert_eq!(
        inspection.node("ok").unwrap().to_json()["states"],
        json!(["hovered", "focused"])
    );
    assert_eq!(state_flag("pressed"), Some(UiStateFlags::PRESSED));
    assert_eq!(state_flag("sparkly"), None);
    assert!(doc.inspect([0, 480], &NullTextBackend, &states).is_err());
}

// ---- rasterizer ----

fn quad(rect: [f32; 4], color: [f32; 4]) -> Mesh {
    let [x, y, w, h] = rect;
    let vertex = |px: f32, py: f32, u: f32, v: f32| Vertex {
        position: [px, py, 0.0],
        uv: [u, v],
        color,
    };
    Mesh {
        normals: vec![],
        vertices: vec![
            vertex(x, y, 0.0, 0.0),
            vertex(x + w, y, 1.0, 0.0),
            vertex(x + w, y + h, 1.0, 1.0),
            vertex(x, y + h, 0.0, 1.0),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

fn draw(id: &str, layer: i32, mesh: Mesh) -> Draw {
    Draw {
        id: id.into(),
        mesh: Arc::new(mesh),
        texture: None,
        layer,
        space: DrawSpace::Ui,
        clip: None,
        state: DrawState::default(),
        motion: None,
        offset: [0.0; 3],
    }
}

fn options(size: [u32; 2]) -> RasterOptions {
    RasterOptions::new(size)
}

const BLACK: [u8; 4] = [0, 0, 0, 255];

#[test]
fn a_solid_quad_fills_its_pixels_and_no_others() {
    let image = rasterize(
        &[draw(
            "q",
            0,
            quad([10.0, 10.0, 20.0, 20.0], [1.0, 0.0, 0.0, 1.0]),
        )],
        &options([40, 40]),
    )
    .unwrap();
    assert_eq!(image.pixel(20, 20), [255, 0, 0, 255]);
    assert_eq!(image.pixel(10, 10), [255, 0, 0, 255]);
    assert_eq!(image.pixel(29, 29), [255, 0, 0, 255]);
    assert_eq!(image.pixel(9, 9), BLACK);
    assert_eq!(image.pixel(30, 30), BLACK);
    assert_eq!(image.pixel(30, 15), BLACK);
    let painted = (0..40)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .filter(|(x, y)| image.pixel(*x, *y) != BLACK)
        .count();
    assert_eq!(
        painted, 400,
        "the diagonal is neither skipped nor drawn twice"
    );
    assert_eq!(painted_bounds(&image, BLACK), Some([10, 10, 30, 30]));
}

#[test]
fn straight_alpha_blends_without_double_hits_on_the_shared_diagonal() {
    let draws = [draw(
        "q",
        0,
        quad([0.0, 0.0, 16.0, 16.0], [1.0, 1.0, 1.0, 0.5]),
    )];
    let image = rasterize(&draws, &options([16, 16])).unwrap();
    for y in 0..16 {
        for x in 0..16 {
            assert_eq!(image.pixel(x, y), [128, 128, 128, 255], "({x},{y})");
        }
    }
    // Two layers compose: 50% white, then 50% blue over that.
    let draws = [
        draw("a", 0, quad([0.0, 0.0, 4.0, 4.0], [1.0, 1.0, 1.0, 0.5])),
        draw("b", 1, quad([0.0, 0.0, 4.0, 4.0], [0.0, 0.0, 1.0, 0.5])),
    ];
    let image = rasterize(&draws, &options([4, 4])).unwrap();
    // 0.5*128/255 -> 64 red, blue 0.5*255 + 0.5*128 -> 192.
    assert_eq!(image.pixel(2, 2), [64, 64, 192, 255]);
    // A transparent clear colour follows the GPU blend: alpha accumulates.
    let mut opts = options([4, 4]);
    opts.clear = [0.0; 4];
    let image = rasterize(&draws[..1], &opts).unwrap();
    assert_eq!(image.pixel(1, 1)[3], 128);
}

#[test]
fn clip_rects_cut_draws() {
    let mut cut = draw("q", 0, quad([0.0, 0.0, 40.0, 40.0], [0.0, 1.0, 0.0, 1.0]));
    cut.clip = Some([10.0, 10.0, 10.0, 15.0]);
    let image = rasterize(&[cut.clone()], &options([40, 40])).unwrap();
    assert_eq!(image.pixel(10, 10), [0, 255, 0, 255]);
    assert_eq!(image.pixel(19, 24), [0, 255, 0, 255]);
    assert_eq!(image.pixel(9, 15), BLACK);
    assert_eq!(image.pixel(20, 15), BLACK);
    assert_eq!(image.pixel(15, 25), BLACK);
    assert_eq!(painted_bounds(&image, BLACK), Some([10, 10, 20, 25]));
    // An empty clip draws nothing.
    cut.clip = Some([10.0, 10.0, 0.0, 15.0]);
    let image = rasterize(&[cut], &options([40, 40])).unwrap();
    assert_eq!(painted_bounds(&image, BLACK), None);
}

#[test]
fn layers_then_ids_decide_the_paint_order() {
    let red = quad([0.0, 0.0, 10.0, 10.0], [1.0, 0.0, 0.0, 1.0]);
    let blue = quad([0.0, 0.0, 10.0, 10.0], [0.0, 0.0, 1.0, 1.0]);
    // Listed top layer first: the layer still wins.
    let draws = [draw("a", 5, red.clone()), draw("b", 1, blue.clone())];
    let image = rasterize(&draws, &options([10, 10])).unwrap();
    assert_eq!(image.pixel(5, 5), [255, 0, 0, 255]);
    // Equal layers: later id paints on top regardless of list order.
    let draws = [draw("z", 0, blue.clone()), draw("a", 0, red.clone())];
    let image = rasterize(&draws, &options([10, 10])).unwrap();
    assert_eq!(image.pixel(5, 5), [0, 0, 255, 255]);
    // Draws outside the UI space are not part of the preview.
    let mut world = draw("w", 9, red);
    world.space = DrawSpace::World;
    let image = rasterize(&[world], &options([10, 10])).unwrap();
    assert_eq!(painted_bounds(&image, BLACK), None);
}

#[test]
fn textures_are_sampled_nearest_or_bilinear_and_tinted() {
    let texture = Arc::new(Texture {
        width: 2,
        height: 1,
        pixels: vec![255, 0, 0, 255, 0, 0, 255, 255],
    });
    let mut textured = draw("t", 0, quad([0.0, 0.0, 8.0, 2.0], [1.0; 4]));
    textured.texture = Some(texture.clone());
    let image = rasterize(&[textured.clone()], &options([8, 2])).unwrap();
    assert_eq!(image.pixel(1, 0), [255, 0, 0, 255]);
    assert_eq!(image.pixel(6, 1), [0, 0, 255, 255]);
    let mut smooth = options([8, 2]);
    smooth.sampling = UiSampling::Bilinear;
    let image = rasterize(&[textured], &smooth).unwrap();
    // The seam between the texels blends; the ends stay pure.
    let mid = image.pixel(3, 0);
    assert!(
        mid[0] > 20 && mid[0] < 235 && mid[2] > 20 && mid[2] < 235,
        "{mid:?}"
    );
    assert_eq!(image.pixel(0, 0), [255, 0, 0, 255]);
    // Vertex colour tints the texel; alpha scales the blend.
    let mut tinted = draw("t", 0, quad([0.0, 0.0, 8.0, 2.0], [0.0, 1.0, 1.0, 1.0]));
    tinted.texture = Some(texture);
    let image = rasterize(&[tinted], &options([8, 2])).unwrap();
    assert_eq!(
        image.pixel(1, 0),
        [0, 0, 0, 255],
        "red texel times a cyan tint"
    );
    assert_eq!(image.pixel(6, 0), [0, 0, 255, 255]);
}

#[test]
fn scale_multiplies_coordinates_and_output_is_deterministic() {
    let draws = [draw(
        "q",
        0,
        quad([2.0, 2.0, 4.0, 4.0], [1.0, 1.0, 0.0, 1.0]),
    )];
    let mut doubled = options([20, 20]);
    doubled.scale = 2.0;
    let image = rasterize(&draws, &doubled).unwrap();
    assert_eq!(painted_bounds(&image, BLACK), Some([4, 4, 12, 12]));
    assert_eq!(
        image.digest(),
        rasterize(&draws, &doubled).unwrap().digest()
    );
    let png = image.to_png().unwrap();
    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (20, 20));
    assert_eq!(decoded.to_rgba8().into_raw(), image.pixels);
}

#[test]
fn hostile_inputs_are_rejected() {
    let draws = [draw("q", 0, quad([0.0, 0.0, 4.0, 4.0], [1.0; 4]))];
    for size in [[0, 4], [4, 0], [MAX_RASTER_DIMENSION + 1, 4]] {
        assert_eq!(
            rasterize(&draws, &options(size)).unwrap_err().diagnostics[0].code,
            "ui.preview.size"
        );
    }
    let mut scaled = options([4, 4]);
    scaled.scale = f32::NAN;
    assert!(rasterize(&draws, &scaled).is_err());
    let mut bad = quad([0.0, 0.0, 4.0, 4.0], [1.0; 4]);
    bad.indices[5] = 99;
    assert_eq!(
        rasterize(&[draw("b", 0, bad)], &options([4, 4]))
            .unwrap_err()
            .diagnostics[0]
            .code,
        "ui.preview.invalid_mesh"
    );
    let mut nan = quad([0.0, 0.0, 4.0, 4.0], [1.0; 4]);
    nan.vertices[0].position[0] = f32::NAN;
    assert!(rasterize(&[draw("n", 0, nan)], &options([4, 4])).is_err());
    let many: Vec<Draw> = (0..=MAX_RASTER_DRAWS)
        .map(|i| draw(&i.to_string(), 0, Mesh::default()))
        .collect();
    assert_eq!(
        rasterize(&many, &options([4, 4])).unwrap_err().diagnostics[0].code,
        "ui.preview.too_complex"
    );
    // Overdraw is bounded by the pixel-visit budget.
    let mut small = options([64, 64]);
    small.max_fill = 4000;
    let wide = [draw("w", 0, quad([0.0, 0.0, 64.0, 64.0], [1.0; 4]))];
    assert_eq!(
        rasterize(&wide, &small).unwrap_err().diagnostics[0].code,
        "ui.preview.too_complex"
    );
    small.max_fill = 2 * 64 * 64;
    assert!(rasterize(&wide, &small).is_ok());
    let huge = RasterOptions::new([4096, 4096]);
    assert_eq!(huge.max_fill, MAX_RASTER_FILL);
}

// ---- previews of real UI ----

fn label_root(font: &str, text: &str) -> UiRoot {
    UiRoot {
        tree: UiTree {
            nodes: vec![UiNode {
                id: "label".into(),
                widget: UiWidget::Label { text: text.into() },
                style: UiStyle {
                    width: UiLength::Pixels(120.0),
                    height: UiLength::Pixels(40.0),
                    padding: 0.0,
                    background: [0.0; 4],
                    foreground: [1.0; 4],
                    font: font.into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
        },
        ..Default::default()
    }
}

#[test]
fn a_bitmap_font_label_draws_glyph_pixels() {
    let fixture = fixture();
    let assets = UiPreviewAssets {
        fonts: fixture.fonts,
        textures: fixture.textures,
        ..Default::default()
    };
    assert!(assets.has_fonts());
    assert_eq!(assets.sole_font().as_deref(), Some("font"));
    let mut view = UiPreviewOptions::new([120, 40]);
    view.clear = [0.0, 0.0, 0.0, 1.0];
    let preview = render_preview(&label_root("font", "AA"), "label", &assets, &view).unwrap();
    assert_eq!((preview.image.width, preview.image.height), (120, 40));
    assert_eq!(
        preview.draws, 1,
        "only the glyph run: the background is transparent"
    );
    let bounds = painted_bounds(&preview.image, BLACK).expect("glyph pixels");
    // Two 4x8 glyphs advancing by 5 start at the label's top-left.
    assert!(bounds[2] - bounds[0] >= 8, "{bounds:?}");
    assert_eq!(
        preview.image.pixel(bounds[0], bounds[1]),
        [255, 255, 255, 255]
    );
    // The default font fills nodes that name none.
    let mut plain = label_root("", "A");
    plain.tree.nodes[0].style.font = String::new();
    let mut with_default = UiPreviewOptions::new([120, 40]);
    with_default.view.default_font = Some("font");
    let preview = render_preview(&plain, "label", &assets, &with_default).unwrap();
    assert!(painted_bounds(&preview.image, [18, 18, 23, 255]).is_some());
    // Scale doubles the image.
    let mut big = view.clone();
    big.scale = 2.0;
    let preview = render_preview(&label_root("font", "AA"), "label", &assets, &big).unwrap();
    assert_eq!((preview.image.width, preview.image.height), (240, 80));
    let bigger = painted_bounds(&preview.image, BLACK).unwrap();
    assert_eq!(bigger[0], bounds[0] * 2);
}

#[test]
fn a_ttf_label_draws_antialiased_glyph_pixels() {
    let mut engine = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    engine
        .register_font(
            epaint_default_fonts::UBUNTU_LIGHT.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(24.0),
                atlas: FontAtlasMode::Coverage,
            },
        )
        .unwrap();
    let assets = UiPreviewAssets {
        engine: Some(Arc::new(engine)),
        ..Default::default()
    };
    assert_eq!(assets.sole_font().as_deref(), Some("ui"));
    let mut view = UiPreviewOptions::new([160, 40]);
    view.clear = [0.0, 0.0, 0.0, 1.0];
    let preview = render_preview(&label_root("ui", "Hello"), "label", &assets, &view).unwrap();
    let image = &preview.image;
    let bounds = painted_bounds(image, BLACK).expect("shaped glyph pixels");
    assert!(
        bounds[2] - bounds[0] > 30 && bounds[3] - bounds[1] > 8,
        "{bounds:?}"
    );
    let mut full = 0;
    let mut partial = 0;
    for y in 0..image.height {
        for x in 0..image.width {
            match image.pixel(x, y)[0] {
                0 => {}
                255 => full += 1,
                _ => partial += 1,
            }
        }
    }
    assert!(
        full > 10 && partial > 10,
        "coverage text anti-aliases: {full} {partial}"
    );
    assert_eq!(
        image.digest(),
        render_preview(&label_root("ui", "Hello"), "label", &assets, &view)
            .unwrap()
            .image
            .digest(),
        "repeat renders are identical"
    );
}

#[test]
fn previews_apply_themes_states_and_bound_the_image() {
    let assets = UiPreviewAssets::default();
    let root: UiRoot = serde_json::from_value(json!({"version": 1, "tree": {"nodes": [
        {"id": "box", "style": {"width": px(20.0), "height": px(20.0), "padding": 0},
            "classes": ["tile"]},
    ]}}))
    .unwrap();
    let theme: UiTheme = serde_json::from_value(json!({"version": 1, "name": "t", "tokens": {},
    "rules": [
        {"selector": ".tile", "style": {"background": [1, 0, 0, 1]}},
        {"selector": ".tile:hover", "style": {"background": [0, 1, 0, 1]}},
    ]}))
    .unwrap();
    let composed = UiComposedTheme::new(&theme).unwrap();
    let mut view = UiPreviewOptions::new([20, 20]);
    view.clear = [0.0, 0.0, 0.0, 1.0];
    let plain = render_preview(&root, "r", &assets, &view).unwrap();
    assert_eq!(
        plain.image.pixel(10, 10),
        [26, 31, 41, 255],
        "the built-in background"
    );
    view.view.theme = Some(&composed);
    let themed = render_preview(&root, "r", &assets, &view).unwrap();
    assert_eq!(themed.image.pixel(10, 10), [255, 0, 0, 255]);
    let mut hover = UiStateFlags::default();
    hover.insert(UiStateFlags::HOVERED);
    view.view.states.insert("box".into(), hover);
    let hovered = render_preview(&root, "r", &assets, &view).unwrap();
    assert_eq!(hovered.image.pixel(10, 10), [0, 255, 0, 255]);

    // The document theme applies without being passed explicitly.
    let mut doc = UiDocument::new(root).unwrap();
    doc.set_theme(Some(theme)).unwrap();
    let mut view = UiPreviewOptions::new([20, 20]);
    view.clear = [0.0, 0.0, 0.0, 1.0];
    assert_eq!(
        doc.preview(&assets, &view).unwrap().image.pixel(10, 10),
        [255, 0, 0, 255]
    );

    for size in [[MAX_RASTER_DIMENSION + 1, 10], [10, 0]] {
        let error =
            render_preview(doc.root(), "r", &assets, &UiPreviewOptions::new(size)).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "ui.preview.size");
    }
    let mut scaled = UiPreviewOptions::new([3000, 10]);
    scaled.scale = 2.0;
    assert_eq!(
        render_preview(doc.root(), "r", &assets, &scaled)
            .unwrap_err()
            .diagnostics[0]
            .code,
        "ui.preview.size"
    );
}

#[test]
fn referenced_textures_cover_images_nine_slices_and_inline_images() {
    let root: UiRoot = serde_json::from_value(json!({"version": 1, "tree": {"nodes": [
        {"id": "a", "widget": {"kind": "image", "texture": "tex:a"}},
        {"id": "b", "widget": {"kind": "nine_slice", "texture": "tex:b", "slices": [1, 1, 1, 1]}},
        {"id": "c", "widget": {"kind": "rich_label", "markup": "x [img=tex:c width=8 height=8] [img=tex:d]"}},
    ]}}))
    .unwrap();
    let keys: Vec<String> = referenced_textures(&root).into_iter().collect();
    assert_eq!(keys, ["tex:a", "tex:b", "tex:c", "tex:d"]);
}

/// With `UI_AUTHORING_DUMP=<file.png>` set, writes a sample preview there for a
/// manual look (the vision check in `docs/ui-authoring.md`); otherwise a no-op.
#[test]
fn dump_a_sample_preview_when_requested() {
    let Ok(path) = std::env::var("UI_AUTHORING_DUMP") else {
        return;
    };
    let mut engine = EngineTextBackend::new(AtlasConfig::default()).unwrap();
    engine
        .register_font(
            epaint_default_fonts::UBUNTU_LIGHT.to_vec(),
            "ui",
            FontFamilySettings {
                size: Some(18.0),
                atlas: FontAtlasMode::Coverage,
            },
        )
        .unwrap();
    let assets = UiPreviewAssets {
        engine: Some(Arc::new(engine)),
        ..Default::default()
    };
    let root: UiRoot = serde_json::from_value(json!({"version": 1, "tree": {"nodes": [
        {"id": "panel", "style": {"width": px(300.0), "height": px(190.0), "layout": "column",
            "padding": 12, "gap": 8, "font": "ui"}},
        {"id": "title", "parent": "panel", "widget": {"kind": "label", "text": "Settings"},
            "style": {"width": {"unit": "fill"}, "height": px(26.0), "padding": 0}},
        {"id": "music", "parent": "panel", "widget": {"kind": "toggle", "text": "Music", "checked": true},
            "style": {"width": {"unit": "fill"}, "height": px(26.0)}},
        {"id": "volume", "parent": "panel", "widget": {"kind": "slider", "value": 0.6, "min": 0,
            "max": 1, "step": 0.1}, "style": {"width": {"unit": "fill"}, "height": px(22.0)}},
        {"id": "load", "parent": "panel", "widget": {"kind": "progress", "value": 0.4, "min": 0,
            "max": 1}, "style": {"width": {"unit": "fill"}, "height": px(14.0)}},
        {"id": "apply", "parent": "panel", "widget": {"kind": "button", "text": "Apply"},
            "style": {"width": px(100.0), "height": px(32.0)}},
    ]}}))
    .unwrap();
    let mut options = UiPreviewOptions::new([324, 214]);
    options.scale = 2.0;
    options.view.default_font = Some("ui");
    options.view.states.insert("apply".into(), {
        let mut flags = UiStateFlags::default();
        flags.insert(UiStateFlags::FOCUSED);
        flags
    });
    let preview = render_preview(&root, "demo", &assets, &options).unwrap();
    std::fs::write(path, preview.image.to_png().unwrap()).unwrap();
}
