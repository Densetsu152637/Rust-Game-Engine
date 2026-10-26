use super::*;
use engine_editor_graph::{GraphCanvas, GraphFile, GraphSession};

pub(super) struct GraphTab {
    pub asset: String,
    pub file: GraphFile,
    pub session: GraphSession,
    pub canvas: GraphCanvas,
    draft_path: String,
    confirm_close: bool,
}
impl PortableEditor {
    pub(super) fn graph_is_dirty(&self) -> bool {
        self.graph
            .as_ref()
            .is_some_and(|tab| tab.session.is_dirty())
    }
    pub(super) fn open_graph(&mut self, asset: &str) -> Result<()> {
        if self.graph.as_ref().is_some_and(|tab| tab.asset == asset) {
            self.cancel_interaction();
            self.graph_view = true;
            return Ok(());
        }
        if self.graph_is_dirty() {
            return Err(anyhow!(
                "Save or explicitly close the modified graph before opening another graph"
            ));
        }
        let project = self
            .controller
            .document()
            .ok_or_else(|| anyhow!("No project open"))?
            .project();
        let asset_entry = project.require_asset(asset, "graph")?;
        let (file, session) = GraphFile::open_registered(&project.root, &asset_entry.path)?;
        self.cancel_interaction();
        self.graph = Some(GraphTab {
            asset: asset.to_owned(),
            file,
            session,
            canvas: Default::default(),
            draft_path: String::new(),
            confirm_close: false,
        });
        self.graph_view = true;
        Ok(())
    }
    pub(super) fn save_graph(&mut self) -> Result<()> {
        let tab = self
            .graph
            .as_mut()
            .ok_or_else(|| anyhow!("No graph open"))?;
        tab.file.save(&mut tab.session)?;
        Ok(())
    }
    pub(super) fn save_all_documents(&mut self) -> Result<()> {
        if self.graph_is_dirty() {
            self.save_graph()?;
        }
        self.controller.save()
    }
    pub(super) fn graph_panel(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = &mut self.graph else {
            ui.heading("Graph editor");
            ui.label("Choose Open graph beside a graph asset in the Assets list.");
            ui.label(format!(
                "Registered graph kinds: {}",
                engine_project::graphs::kinds()
                    .names()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return;
        };
        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading(&tab.asset);
            ui.label(tab.file.relative().display().to_string());
            if ui.button("Close graph").clicked() {
                if tab.session.is_dirty() {
                    tab.confirm_close = true;
                } else {
                    close = true;
                }
            }
        });
        if tab.confirm_close {
            ui.horizontal_wrapped(|ui| {
                ui.label("Save graph changes before closing?");
                if ui.button("Save and close graph").clicked() {
                    match tab.file.save(&mut tab.session) {
                        Ok(()) => close = true,
                        Err(error) => self.error = error.to_string(),
                    }
                }
                if ui.button("Discard graph changes").clicked() {
                    close = true;
                }
                if ui.button("Keep editing graph").clicked() {
                    tab.confirm_close = false;
                }
            });
        }
        egui::CollapsingHeader::new("Export incomplete draft").show(ui, |ui| {
            ui.label(
                "New project-relative .json path; draft is not registered as an active asset.",
            );
            ui.text_edit_singleline(&mut tab.draft_path);
            if ui.button("Export draft").clicked() {
                match tab
                    .file
                    .export_draft(Path::new(&tab.draft_path), &tab.session)
                {
                    Ok(()) => self.error.clear(),
                    Err(error) => self.error = error.to_string(),
                }
            }
        });
        tab.canvas.show(ui, &mut tab.session);
        if close {
            self.graph = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portable::tests::{Host, fixture};
    use eframe::App;

    #[test]
    fn real_graph_asset_open_action_reaches_canvas_and_saves_separately_from_scene() {
        let dir = fixture();
        let document = serde_json::json!({"version":1,"kind":"behavior_tree","nodes":[{"id":"root","node_type":"root","position":[30,40]},{"id":"wait","node_type":"wait","params":{"seconds":1.0},"position":[250,40]}],"edges":[{"from":{"node":"root","port":"child"},"to":{"node":"wait","port":"parent"}}]});
        std::fs::write(dir.path().join("graph.json"), document.to_string()).unwrap();
        let manifest = dir.path().join("project.json");
        let mut project: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        project["assets"] = serde_json::json!([{"id":"brain","kind":"graph","path":"graph.json"}]);
        std::fs::write(&manifest, project.to_string()).unwrap();
        let mut controller = EditorController::new(Box::new(Host));
        controller.open(&manifest, false).unwrap();
        let mut app = PortableEditor::new(controller);
        app.renderer = Some(OffscreenSprites::headless(640, 360, RenderLimits::default()).unwrap());
        let scene = app.controller.document().unwrap().snapshot().clone();
        let ctx = egui::Context::default();
        let frame = |app: &mut PortableEditor, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
            )
        };
        let output = frame(&mut app, vec![]);
        let target = output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::epaint::Shape::Text(t) = &shape.shape {
                    (t.galley.job.text == "Open graph brain")
                        .then_some(t.pos + t.galley.size() / 2.0)
                } else {
                    None
                }
            })
            .expect("reachable graph asset action");
        for pressed in [true, false] {
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(target),
                    egui::Event::PointerButton {
                        pos: target,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        assert!(app.graph_view, "{}", app.error);
        assert_eq!(app.graph.as_ref().unwrap().asset, "brain");
        assert!(app.graph.as_ref().unwrap().session.diagnostics().is_empty());
        let output = frame(&mut app, vec![]);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(t) if t.galley.job.text == "root: root")));
        let tab = app.graph.as_mut().unwrap();
        tab.session
            .apply(&[serde_json::from_value(
                serde_json::json!({"op":"set_position","node":"wait","position":[300,80]}),
            )
            .unwrap()])
            .unwrap();
        tab.file
            .export_draft(Path::new("draft.json"), &tab.session)
            .unwrap();
        assert!(app.graph_is_dirty());
        assert!(app.open_graph("another").is_err());
        app.save_graph().unwrap();
        assert!(!app.graph_is_dirty());
        let (_, reopened) =
            GraphFile::open_registered(dir.path(), Path::new("graph.json")).unwrap();
        assert_eq!(
            reopened.document().node("wait").unwrap().position,
            Some([300.0, 80.0])
        );
        assert_eq!(app.controller.document().unwrap().snapshot(), &scene);
    }
}
