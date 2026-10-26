use super::*;
use engine_editor_settings::{GizmoMode, GizmoPivot, GizmoSpace};
impl PortableEditor {
    pub(super) fn settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (mode, label) in [
                (GizmoMode::Translate, "Move (W)"),
                (GizmoMode::Rotate, "Rotate (E)"),
                (GizmoMode::Scale, "Scale (R)"),
            ] {
                self.settings_dirty |= ui
                    .selectable_value(&mut self.settings.gizmo.mode, mode, label)
                    .changed();
            }
            self.settings_dirty |= ui
                .checkbox(&mut self.settings.snapping.enabled, "Snap")
                .changed();
            self.settings_dirty |= ui
                .checkbox(&mut self.settings.grid.visible, "Grid")
                .changed();
            egui::ComboBox::from_id_salt("space")
                .selected_text(format!("{:?}", self.settings.gizmo.space))
                .show_ui(ui, |ui| {
                    for value in [GizmoSpace::World, GizmoSpace::Local] {
                        self.settings_dirty |= ui
                            .selectable_value(
                                &mut self.settings.gizmo.space,
                                value,
                                format!("{value:?}"),
                            )
                            .changed();
                    }
                });
            egui::ComboBox::from_id_salt("pivot")
                .selected_text(format!("{:?}", self.settings.gizmo.pivot))
                .show_ui(ui, |ui| {
                    for value in [GizmoPivot::Center, GizmoPivot::Active] {
                        self.settings_dirty |= ui
                            .selectable_value(
                                &mut self.settings.gizmo.pivot,
                                value,
                                format!("{value:?}"),
                            )
                            .changed();
                    }
                });
            ui.menu_button("View settings", |ui| {
                for (label, value, range) in [
                    (
                        "Move step",
                        &mut self.settings.snapping.translate_step,
                        0.001..=1000.0,
                    ),
                    (
                        "Angle step",
                        &mut self.settings.snapping.rotate_degrees,
                        0.1..=180.0,
                    ),
                    (
                        "Scale step",
                        &mut self.settings.snapping.scale_step,
                        0.001..=10.0,
                    ),
                    (
                        "Grid spacing",
                        &mut self.settings.grid.spacing,
                        0.001..=1000.0,
                    ),
                ] {
                    ui.label(label);
                    self.settings_dirty |= ui
                        .add(egui::DragValue::new(value).speed(0.01).range(range))
                        .changed();
                }
                ui.text_edit_singleline(&mut self.bookmark_name);
                if ui.button("Save camera bookmark").clicked() {
                    match self
                        .settings
                        .save_bookmark(self.navigation.to_bookmark(&self.bookmark_name))
                    {
                        Ok(()) => self.settings_dirty = true,
                        Err(e) => self.error = e.to_string(),
                    }
                }
                for bookmark in self.settings.bookmarks.clone() {
                    ui.horizontal(|ui| {
                        if ui.button(&bookmark.name).clicked() {
                            self.cancel_interaction();
                            self.navigation.apply_bookmark(&bookmark);
                            self.dirty_preview = true;
                        }
                        if ui.small_button("Delete").clicked() {
                            self.settings.delete_bookmark(&bookmark.name);
                            self.settings_dirty = true;
                        }
                    });
                }
            });
            if self.settings_dirty
                && self.settings_valid
                && ui.button("Save view settings").clicked()
                && let Some(document) = self.controller.document()
            {
                match self.settings.save(&document.project().root) {
                    Ok(()) => self.settings_dirty = false,
                    Err(e) => self.error = e.to_string(),
                }
            }
            if !self.settings_valid {
                ui.label("Invalid editor.json: repair file before saving settings");
            }
        });
    }
}

impl PortableEditor {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Editor");
            if ui.selectable_label(!self.graph_view, "Scene").clicked() {
                if let Some(tab) = &mut self.graph {
                    tab.canvas.cancel_gesture();
                }
                self.graph_view = false;
            }
            if ui.selectable_label(self.graph_view, "Graph").clicked() {
                self.cancel_interaction();
                self.graph_view = true;
            }
            if ui.button("Save").clicked() {
                let r = if self.graph_view {
                    self.save_graph()
                } else {
                    self.controller.save()
                };
                self.report(r);
            }
            if ui.button("Undo").clicked() {
                let r = if self.graph_view {
                    if let Some(tab) = &mut self.graph {
                        tab.canvas.cancel_gesture();
                        tab.session.undo();
                    }
                    Ok(())
                } else {
                    self.controller.undo()
                };
                self.report(r);
            }
            if ui.button("Redo").clicked() {
                let r = if self.graph_view {
                    if let Some(tab) = &mut self.graph {
                        tab.canvas.cancel_gesture();
                        tab.session.redo();
                    }
                    Ok(())
                } else {
                    self.controller.redo()
                };
                self.report(r);
            }
            if ui
                .button(if self.controller.is_playing() {
                    "Stop"
                } else {
                    "Play"
                })
                .clicked()
            {
                let r = if self.controller.is_playing() {
                    self.controller.stop()
                } else {
                    self.controller.play()
                };
                self.report(r);
            }
            let reload = ui.add_enabled(
                self.controller.can_reload_play(),
                egui::Button::new("Reload game"),
            );
            let cancel = ui.add_enabled(
                self.controller.is_reloading_play(),
                egui::Button::new("Cancel reload"),
            );
            #[cfg(test)]
            {
                self.play_control_rects = Some([reload.rect, cancel.rect]);
            }
            if reload.clicked() {
                let result = self.controller.reload_play();
                self.report(result);
            }
            if cancel.clicked() {
                let result = self.controller.cancel_play_reload();
                self.report(result);
            }
            if ui
                .selectable_label(!self.navigation.three_dimensional, "2D")
                .clicked()
            {
                self.navigation.three_dimensional = false;
                self.dirty_preview = true;
            }
            if ui
                .selectable_label(self.navigation.three_dimensional, "3D")
                .clicked()
            {
                self.navigation.three_dimensional = true;
                self.dirty_preview = true;
            }
            if ui.button("Frame selection").clicked() {
                if let Some(entity) = self.selected()
                    && let Some(position) = self.world_position(&entity)
                {
                    self.navigation.center = position;
                }
                self.dirty_preview = true;
            }
            if self.controller.is_dirty() || self.graph_is_dirty() {
                ui.label("Unsaved");
            }
        });
        if !self.graph_view {
            self.settings_panel(ui);
        }
    }
    pub(super) fn hierarchy(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        let entities = self
            .controller
            .document()
            .map(|d| d.snapshot().entities.clone())
            .unwrap_or_default();
        for entity in entities {
            if ui
                .selectable_label(self.controller.is_selected(&entity.id), &entity.name)
                .clicked()
            {
                let r = selection::apply_click(
                    &mut self.controller,
                    Some(entity.id),
                    selection::action_for(ui.input(|i| i.modifiers)),
                );
                self.report(r);
            }
        }
        ui.separator();
        ui.heading("Assets");
        let assets = self
            .controller
            .document()
            .map(|document| {
                document
                    .project()
                    .assets
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for asset in assets {
                ui.label(format!("{} · {}", asset.kind, asset.id));
                if asset.kind == "graph" && ui.button(format!("Open graph {}", asset.id)).clicked()
                {
                    let result = self.open_graph(&asset.id);
                    self.report(result);
                }
            }
        });
    }
    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        let Some(mut entity) = self.selected() else {
            ui.label("Select an entity");
            return;
        };
        let key = format!(
            "{}:{}",
            entity.id,
            self.controller
                .document()
                .map(|d| d.revision())
                .unwrap_or("")
        );
        if self.inspected != key {
            self.entity_json = serde_json::to_string_pretty(&entity).unwrap_or_default();
            self.inspected = key;
        }
        ui.label(&entity.name);
        if let Some(transform) = &mut entity.components.transform {
            let mut changed = false;
            ui.label("Position");
            ui.horizontal(|ui| {
                for value in &mut transform.position {
                    changed |= ui.add(egui::DragValue::new(value).speed(0.05)).changed();
                }
            });
            ui.label("Rotation · quaternion XYZW");
            ui.horizontal(|ui| {
                for value in &mut transform.rotation {
                    changed |= ui.add(egui::DragValue::new(value).speed(0.01)).changed();
                }
            });
            ui.label("Scale");
            ui.horizontal(|ui| {
                for value in &mut transform.scale {
                    changed |= ui.add(egui::DragValue::new(value).speed(0.05)).changed();
                }
            });
            if changed {
                self.apply_entity(entity.clone());
            }
        }
        ui.separator();
        ui.label("Entity and components");
        ui.small("Validated JSON edits support every built-in and registered component.");
        egui::ScrollArea::vertical()
            .max_height(390.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.entity_json)
                        .code_editor()
                        .desired_rows(22),
                );
            });
        if ui.button("Apply entity").clicked() {
            match serde_json::from_str(&self.entity_json) {
                Ok(entity) => self.apply_entity(entity),
                Err(error) => self.error = error.to_string(),
            }
        }
    }
}
