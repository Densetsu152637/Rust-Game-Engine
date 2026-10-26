use crate::GraphSession;
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use engine_graph::{GraphDiagnostic, GraphEdit, GraphVariable, NodeId, PortDirection, PortRef};
use serde_json::Value;
use std::collections::BTreeMap;

const WIDTH: f32 = 190.0;
const HEADER: f32 = 30.0;
const ROW: f32 = 25.0;

/// Canvas-local selection, navigation and gesture previews. Graph data lives only in the session.
#[derive(Default)]
pub struct GraphCanvas {
    selected: Option<NodeId>,
    wire: Option<PortRef>,
    drag: Option<(NodeId, [f32; 2], Pos2, Vec2)>,
    pan: Vec2,
    next_id: u64,
    error: Vec<GraphDiagnostic>,
    param_text: BTreeMap<String, String>,
    variable_text: String,
    metadata_key: String,
    metadata_text: String,
}
impl GraphCanvas {
    pub fn selected(&self) -> Option<&NodeId> {
        self.selected.as_ref()
    }
    pub fn errors(&self) -> &[GraphDiagnostic] {
        &self.error
    }
    pub fn cancel_gesture(&mut self) {
        self.drag = None;
        self.wire = None;
        self.param_text.clear();
    }
    fn commit(&mut self, session: &mut GraphSession, edits: &[GraphEdit]) {
        self.error = session.apply(edits).err().unwrap_or_default();
    }
    fn select(&mut self, id: Option<NodeId>) {
        if self.selected != id {
            self.param_text.clear();
        }
        self.selected = id;
    }
    /// Draw inside any native egui panel. A host owns project/file tabs and save dialogs.
    pub fn show(&mut self, ui: &mut egui::Ui, session: &mut GraphSession) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(&session.kind().name);
            if session.is_dirty() {
                ui.label("Modified");
            }
            if session.is_read_only() {
                ui.label("Read only");
            }
            if ui
                .add_enabled(session.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.cancel_gesture();
                session.undo();
                self.param_text.clear();
            }
            if ui
                .add_enabled(session.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.cancel_gesture();
                session.redo();
                self.param_text.clear();
            }
            if ui.button("Reset view").clicked() {
                self.pan = Vec2::ZERO;
            }
            ui.label("Drag headers to move; drag output to input to wire; middle drag to pan.");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Add node:");
            let definitions: Vec<_> = session.kind().registry.iter().cloned().collect();
            for def in definitions {
                if ui
                    .add_enabled(!session.is_read_only(), egui::Button::new(&def.name))
                    .on_hover_text(format!("{}: {}", def.category, def.description))
                    .clicked()
                {
                    let id = loop {
                        self.next_id += 1;
                        let id = format!("node_{}", self.next_id);
                        if session.document().node(&id).is_none() {
                            break NodeId(id);
                        }
                    };
                    let params = def
                        .params
                        .iter()
                        .filter_map(|p| p.default.clone().map(|v| (p.name.clone(), v)))
                        .collect();
                    self.commit(
                        session,
                        &[GraphEdit::AddNode {
                            id: id.clone(),
                            node_type: def.name,
                            params,
                            position: Some([
                                30.0 + (session.document().nodes.len() % 4) as f32 * 215.0,
                                40.0,
                            ]),
                            ports: None,
                        }],
                    );
                    if session.document().node(id.as_str()).is_some() {
                        self.select(Some(id));
                    }
                }
            }
        });
        let keyboard = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.command && i.key_pressed(egui::Key::Z),
                i.modifiers.shift,
            )
        });
        if keyboard.1 {
            self.cancel_gesture();
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            if keyboard.2 {
                self.cancel_gesture();
                if keyboard.3 {
                    session.redo();
                } else {
                    session.undo();
                }
                self.param_text.clear();
            }
            if keyboard.0 {
                self.delete_selected(session);
            }
        }
        egui::CollapsingHeader::new("Properties, variables and connections")
            .default_open(true)
            .show(ui, |ui| self.inspector(ui, session));
        let diagnostics = session.diagnostics();
        egui::CollapsingHeader::new(format!(
            "Diagnostics ({})",
            diagnostics.len() + self.error.len()
        ))
        .show(ui, |ui| {
            for diagnostic in self.error.iter().chain(&diagnostics) {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    format!(
                        "{} {}: {}",
                        diagnostic.code, diagnostic.path, diagnostic.message
                    ),
                );
            }
        });
        self.canvas(ui, session);
    }
    fn delete_selected(&mut self, session: &mut GraphSession) {
        if let Some(id) = self.selected.clone() {
            self.cancel_gesture();
            self.commit(session, &[GraphEdit::RemoveNode { id }]);
            if self.error.is_empty() {
                self.select(None);
            }
        }
    }
    fn inspector(&mut self, ui: &mut egui::Ui, session: &mut GraphSession) {
        ui.add_enabled_ui(!session.is_read_only(), |ui| {
            if let Some(node) = self
                .selected
                .as_ref()
                .and_then(|id| session.document().node(id.as_str()))
                .cloned()
            {
                ui.horizontal(|ui| {
                    ui.label(format!("{} ({})", node.id, node.node_type));
                    if ui.button("Delete node").clicked() {
                        self.delete_selected(session);
                    }
                });
                let params = session
                    .kind()
                    .registry
                    .get(&node.node_type)
                    .map(|def| def.params.clone())
                    .unwrap_or_default();
                for param in params {
                    let initial = node
                        .params
                        .get(&param.name)
                        .or(param.default.as_ref())
                        .unwrap_or(&Value::Null)
                        .to_string();
                    let text = self.param_text.entry(param.name.clone()).or_insert(initial);
                    let mut apply = false;
                    ui.horizontal(|ui| {
                        ui.label(&param.name)
                            .on_hover_text(format!("{:?}. {}", param.ty, param.description));
                        ui.text_edit_singleline(text);
                        apply = ui.button("Apply").clicked();
                    });
                    if apply {
                        match serde_json::from_str(text) {
                            Ok(value) => self.commit(
                                session,
                                &[GraphEdit::SetParam {
                                    node: node.id.clone(),
                                    name: param.name,
                                    value,
                                }],
                            ),
                            Err(e) => {
                                self.error = vec![GraphDiagnostic::new(
                                    "graph.editor.json",
                                    "",
                                    e.to_string(),
                                )]
                            }
                        }
                    }
                }
                ui.small("Parameter values use JSON: quote strings; null restores the default.");
            }
            let edges = session.document().edges.clone();
            for edge in edges {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{}.{} → {}.{}",
                        edge.from.node, edge.from.port, edge.to.node, edge.to.port
                    ));
                    if ui.button("Disconnect").clicked() {
                        self.commit(
                            session,
                            &[GraphEdit::Disconnect {
                                from: edge.from,
                                to: edge.to,
                            }],
                        );
                    }
                });
            }
            egui::CollapsingHeader::new("Variables").show(ui, |ui| {
                for variable in session.document().variables.clone() {
                    ui.horizontal(|ui| {
                        if ui.button(format!("Edit {}", variable.name)).clicked() {
                            self.variable_text =
                                serde_json::to_string(&variable).expect("plain variable");
                        }
                        if ui.button(format!("Remove {}", variable.name)).clicked() {
                            self.commit(
                                session,
                                &[GraphEdit::RemoveVariable {
                                    name: variable.name,
                                }],
                            );
                        }
                    });
                }
                ui.label("Variable JSON {name, type, default?, description?}:");
                ui.text_edit_singleline(&mut self.variable_text);
                if ui.button("Apply variable").clicked() {
                    match serde_json::from_str::<GraphVariable>(&self.variable_text) {
                        Ok(variable) => {
                            let edit = if session.document().variable(&variable.name).is_some() {
                                GraphEdit::SetVariable { variable }
                            } else {
                                GraphEdit::AddVariable { variable }
                            };
                            self.commit(session, &[edit]);
                        }
                        Err(e) => {
                            self.error = vec![GraphDiagnostic::new(
                                "graph.editor.json",
                                "/variables",
                                e.to_string(),
                            )]
                        }
                    }
                }
            });
            egui::CollapsingHeader::new("Metadata").show(ui, |ui| {
                ui.label(format!("{:?}", session.document().metadata));
                ui.text_edit_singleline(&mut self.metadata_key);
                ui.text_edit_singleline(&mut self.metadata_text);
                if ui.button("Apply metadata (null removes)").clicked() {
                    match serde_json::from_str(&self.metadata_text) {
                        Ok(value) => self.commit(
                            session,
                            &[GraphEdit::SetMetadata {
                                key: self.metadata_key.clone(),
                                value,
                            }],
                        ),
                        Err(e) => {
                            self.error = vec![GraphDiagnostic::new(
                                "graph.editor.json",
                                "/metadata",
                                e.to_string(),
                            )]
                        }
                    }
                }
            });
        });
    }
    fn canvas(&mut self, ui: &mut egui::Ui, session: &mut GraphSession) {
        let (rect, background) = ui.allocate_exact_size(
            ui.available_size().max(Vec2::new(200.0, 250.0)),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(23, 26, 32));
        if background.dragged_by(egui::PointerButton::Middle) {
            self.pan += ui.input(|i| i.pointer.delta());
        }
        if background.clicked() {
            self.select(None);
        }
        let origin = rect.min + self.pan;
        let mut ports = BTreeMap::new();
        let nodes = session.document().nodes.clone();
        for (index, node) in nodes.iter().enumerate() {
            let mut position = node.position.unwrap_or([
                30.0 + (index % 4) as f32 * 215.0,
                40.0 + (index / 4) as f32 * 230.0,
            ]);
            if let Some((id, start, _, delta)) = &self.drag {
                if *id == node.id {
                    position = [start[0] + delta.x, start[1] + delta.y];
                }
            }
            let effective = if node.ports.is_empty() {
                session
                    .kind()
                    .registry
                    .get(&node.node_type)
                    .map(|d| d.declared_ports())
                    .unwrap_or_default()
            } else {
                node.ports.clone()
            };
            let node_rect = Rect::from_min_size(
                origin + Vec2::from(position),
                Vec2::new(WIDTH, HEADER + ROW * effective.len().max(1) as f32),
            );
            painter.rect_filled(node_rect, 5.0, Color32::from_rgb(45, 51, 63));
            painter.rect_stroke(
                node_rect,
                5.0,
                Stroke::new(
                    1.5,
                    if self.selected.as_ref() == Some(&node.id) {
                        Color32::LIGHT_BLUE
                    } else {
                        Color32::GRAY
                    },
                ),
                StrokeKind::Inside,
            );
            let header = Rect::from_min_size(node_rect.min, Vec2::new(WIDTH, HEADER));
            painter.text(
                header.center(),
                Align2::CENTER_CENTER,
                format!("{}: {}", node.id, node.node_type),
                FontId::proportional(13.0),
                Color32::WHITE,
            );
            let response = ui.interact(
                header,
                Id::unique(("graph_node", node.id.as_str())),
                Sense::click_and_drag(),
            );
            if response.clicked() || response.drag_started() {
                self.select(Some(node.id.clone()));
            }
            if response.drag_started() && !session.is_read_only() {
                if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                    self.drag = Some((node.id.clone(), position, origin, Vec2::ZERO));
                }
            }
            if response.dragged() || response.drag_stopped() {
                if let Some((id, _, origin, delta)) = &mut self.drag {
                    if *id == node.id {
                        // Absolute displacement survives idle frames and includes release motion.
                        if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                            *delta = pointer - *origin;
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
            if response.drag_stopped() {
                if let Some((id, start, _, delta)) = self.drag.take() {
                    self.commit(
                        session,
                        &[GraphEdit::SetPosition {
                            node: id,
                            position: Some([start[0] + delta.x, start[1] + delta.y]),
                        }],
                    );
                }
            }
            for (i, port) in effective.iter().enumerate() {
                let input = port.direction == PortDirection::In;
                let center = Pos2::new(
                    if input {
                        node_rect.left()
                    } else {
                        node_rect.right()
                    },
                    node_rect.top() + HEADER + ROW * (i as f32 + 0.5),
                );
                let reference = PortRef {
                    node: node.id.clone(),
                    port: port.name.clone(),
                };
                ports.insert(reference.clone(), center);
                painter.circle_filled(
                    center,
                    6.0,
                    if input {
                        Color32::LIGHT_GREEN
                    } else {
                        Color32::LIGHT_BLUE
                    },
                );
                painter.text(
                    center + Vec2::new(if input { 11.0 } else { -11.0 }, 0.0),
                    if input {
                        Align2::LEFT_CENTER
                    } else {
                        Align2::RIGHT_CENTER
                    },
                    format!("{} {:?}", port.name, port.ty),
                    FontId::proportional(11.0),
                    Color32::WHITE,
                );
                let hit = Rect::from_center_size(center, Vec2::splat(18.0));
                let response = ui.interact(
                    hit,
                    Id::unique(("graph_port", node.id.as_str(), &port.name)),
                    Sense::click_and_drag(),
                );
                if !session.is_read_only() {
                    if !input && (response.drag_started() || response.clicked()) {
                        self.wire = Some(reference.clone());
                    }
                    let released = ui.input(|i| {
                        i.pointer.any_released()
                            && i.pointer.interact_pos().is_some_and(|p| hit.contains(p))
                    });
                    if input && (response.clicked() || released) {
                        if let Some(from) = self.wire.take() {
                            self.commit(
                                session,
                                &[GraphEdit::Connect {
                                    from,
                                    to: reference,
                                    index: None,
                                }],
                            );
                        }
                    }
                }
            }
        }
        for edge in &session.document().edges {
            if let (Some(from), Some(to)) = (ports.get(&edge.from), ports.get(&edge.to)) {
                painter.line_segment([*from, *to], Stroke::new(2.0, Color32::LIGHT_BLUE));
            }
        }
        if let Some(from) = self.wire.as_ref().and_then(|p| ports.get(p)) {
            if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                painter.line_segment([*from, pointer], Stroke::new(2.0, Color32::YELLOW));
            }
        }
        if session.is_read_only() {
            self.cancel_gesture();
        }
    }
}
