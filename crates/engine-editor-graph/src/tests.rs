use crate::{FileError, GraphCanvas, GraphFile, GraphSession};
use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use engine_graph::{
    GraphDocument, GraphEdit, GraphKind, NodeTypeDef, NodeTypeRegistry, PortDef, PortRef, PortType,
};

fn kind() -> GraphKind {
    GraphKind::new(
        "test",
        "generic test",
        NodeTypeRegistry::new().with(
            NodeTypeDef::new("pass", "test", "Pass data")
                .port(PortDef::input("in", PortType::Float))
                .port(PortDef::output("out", PortType::Float)),
        ),
    )
}
fn add(id: &str, position: [f32; 2]) -> GraphEdit {
    GraphEdit::AddNode {
        id: id.into(),
        node_type: "pass".into(),
        params: Default::default(),
        position: Some(position),
    }
}
fn session() -> GraphSession {
    GraphSession::new(GraphDocument::new("test"), kind()).unwrap()
}

#[test]
fn atomic_failures_and_history_preserve_document() {
    let mut session = session();
    session.apply(&[add("a", [0.0, 0.0])]).unwrap();
    let baseline = session.document().clone();
    assert!(
        session
            .apply(&[add("b", [0.0, 0.0]), add("a", [0.0, 0.0])])
            .is_err()
    );
    assert_eq!(session.document(), &baseline);
    session.undo();
    assert!(session.document().nodes.is_empty());
    session.redo();
    assert_eq!(session.document(), &baseline);
    session.set_read_only(true);
    assert!(session.apply(&[add("b", [0.0, 0.0])]).is_err());
    session.undo();
    assert_eq!(session.document(), &baseline);
}
#[test]
fn save_reopen_conflict_and_draft_policy() {
    let root = tempfile::tempdir().unwrap();
    let (mut file, mut session) =
        GraphFile::create(root.path(), "graph.json".as_ref(), &kind()).unwrap();
    session.apply(&[add("a", [41.0, 72.0])]).unwrap();
    file.save(&mut session).unwrap();
    assert!(!session.is_dirty());
    let (_, loaded) = GraphFile::open(root.path(), "graph.json".as_ref(), &kind()).unwrap();
    assert_eq!(loaded.document(), session.document());
    session
        .apply(&[GraphEdit::SetPosition {
            node: "a".into(),
            position: Some([55.0, 72.0]),
        }])
        .unwrap();
    std::fs::write(root.path().join("graph.json"), b"external").unwrap();
    assert!(matches!(file.save(&mut session), Err(FileError::Conflict)));
    assert!(session.is_dirty());
    assert_eq!(
        std::fs::read(root.path().join("graph.json")).unwrap(),
        b"external"
    );
    assert!(file.export_draft("graph.json".as_ref(), &session).is_err());
    file.export_draft("draft.json".as_ref(), &session).unwrap();
    assert!(session.is_dirty());
    assert!(GraphFile::create(root.path(), "../escape.json".as_ref(), &kind()).is_err());
}
#[test]
fn all_shipped_kinds_keep_their_palettes_and_compiler_diagnostics() {
    let kinds = engine_project::graphs::kinds();
    for name in ["animation", "behavior_tree", "state_tree"] {
        let kind = kinds.get(name).unwrap();
        let session = GraphSession::new(GraphDocument::new(name), kind.clone()).unwrap();
        assert!(!session.kind().registry.is_empty());
        assert_eq!(
            session.diagnostics(),
            engine_project::graphs::validate_document(
                session.document(),
                engine_graph::Strictness::Complete
            )
        );
    }
    let root = tempfile::tempdir().unwrap();
    let kind = kinds.get("behavior_tree").unwrap();
    let (mut file, mut session) =
        GraphFile::create(root.path(), "active.json".as_ref(), kind).unwrap();
    assert!(matches!(
        file.save(&mut session),
        Err(FileError::Diagnostics(_))
    ));
    assert!(!root.path().join("active.json").exists());
    file.export_draft("work.json".as_ref(), &session).unwrap();
}

struct Harness {
    ctx: egui::Context,
    canvas: GraphCanvas,
    session: GraphSession,
    time: f64,
}
impl Harness {
    fn new() -> Self {
        Self {
            ctx: Default::default(),
            canvas: Default::default(),
            session: session(),
            time: 0.0,
        }
    }
    fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.time += 0.02;
        self.ctx.run_pass(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 900.0))),
                events,
                time: Some(self.time),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| self.canvas.show(ui, &mut self.session));
            },
        )
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            },
        ]);
    }
    fn click(&mut self, at: Pos2) {
        self.pointer(at, true);
        self.pointer(at, false);
    }
    fn text(&mut self, text: &str) -> Pos2 {
        let output = self.frame(Vec::new());
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::epaint::Shape::Text(t) = &shape.shape {
                    if t.galley.job.text == text {
                        Some(t.pos + t.galley.size() / 2.0)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("Missing text: {text}"))
    }
}
#[test]
fn real_egui_palette_drag_wire_delete_and_one_undo_per_gesture() {
    let mut h = Harness::new();
    let palette = h.text("pass");
    h.click(palette);
    assert_eq!(h.session.document().nodes.len(), 1);
    h.click(palette);
    assert_eq!(h.session.document().nodes.len(), 2);
    let first = h.text("node_1: pass");
    let old = h.session.document().nodes[0].position.unwrap();
    h.pointer(first, true);
    h.frame(vec![Event::PointerMoved(first + Vec2::new(30.0, 20.0))]);
    h.frame(vec![Event::PointerMoved(first + Vec2::new(60.0, 40.0))]);
    h.frame(Vec::new()); // A stationary frame must preserve the gesture displacement.
    // Preview only, prior graph is authoritative until mouse release.
    assert_eq!(h.session.document().nodes[0].position, Some(old));
    h.pointer(first + Vec2::new(75.0, 50.0), false);
    let moved = h.session.document().nodes[0].position.unwrap();
    assert_eq!(moved, [old[0] + 75.0, old[1] + 50.0]);
    h.session.undo();
    assert_eq!(h.session.document().nodes[0].position, Some(old));
    h.session.redo();
    assert_eq!(h.session.document().nodes[0].position, Some(moved));
    let first = h.text("node_1: pass");
    let second = h.text("node_2: pass");
    let output = first + Vec2::new(95.0, 52.5);
    let input = second + Vec2::new(-95.0, 27.5);
    h.pointer(output, true);
    h.frame(vec![Event::PointerMoved(input)]);
    h.pointer(input, false);
    assert_eq!(h.session.document().edges.len(), 1);
    assert_eq!(
        h.session.document().edges[0].from,
        PortRef::new("node_1", "out")
    );
    let second = h.text("node_2: pass");
    h.click(second);
    let delete = h.text("Delete node");
    h.click(delete);
    assert_eq!(h.session.document().nodes.len(), 1);
    assert!(h.session.document().edges.is_empty());
    h.session.undo();
    assert_eq!(h.session.document().nodes.len(), 2);
    assert_eq!(h.session.document().edges.len(), 1);
}

#[test]
fn bounded_reads_and_failed_readonly_save_leave_baseline_intact() {
    let root = tempfile::tempdir().unwrap();
    let large = root.path().join("large.json");
    std::fs::write(&large, vec![b' '; engine_graph::MAX_DOCUMENT_BYTES + 1]).unwrap();
    assert!(matches!(
        GraphFile::open(root.path(), "large.json".as_ref(), &kind()),
        Err(FileError::TooLarge)
    ));
    let (mut file, mut session) =
        GraphFile::create(root.path(), "graph.json".as_ref(), &kind()).unwrap();
    session.apply(&[add("a", [1.0, 2.0])]).unwrap();
    file.save(&mut session).unwrap();
    let baseline = std::fs::read(root.path().join("graph.json")).unwrap();
    session
        .apply(&[GraphEdit::SetPosition {
            node: "a".into(),
            position: Some([3.0, 4.0]),
        }])
        .unwrap();
    session.set_read_only(true);
    assert!(matches!(file.save(&mut session), Err(FileError::ReadOnly)));
    assert!(session.is_dirty());
    assert_eq!(
        std::fs::read(root.path().join("graph.json")).unwrap(),
        baseline
    );
}

#[test]
fn incompatible_ports_reject_connection_without_consuming_undo() {
    let mut kind = kind();
    kind.registry
        .register(
            NodeTypeDef::new("boolean", "test", "Boolean input")
                .port(PortDef::input("in", PortType::Bool)),
        )
        .unwrap();
    let mut session = GraphSession::new(GraphDocument::new("test"), kind).unwrap();
    session
        .apply(&[
            add("a", [0.0, 0.0]),
            GraphEdit::AddNode {
                id: "b".into(),
                node_type: "boolean".into(),
                params: Default::default(),
                position: None,
            },
        ])
        .unwrap();
    let baseline = session.document().clone();
    let errors = session
        .apply(&[GraphEdit::Connect {
            from: PortRef::new("a", "out"),
            to: PortRef::new("b", "in"),
            index: None,
        }])
        .unwrap_err();
    assert!(errors.iter().any(|d| d.code == "graph.edge.type_mismatch"));
    assert_eq!(session.document(), &baseline);
    session.undo();
    assert!(session.document().nodes.is_empty());
}
