//! Portable authoring shell. Simulation and play ownership stay in the controller.
use crate::{EditorController, PlayHost};
use eframe::egui;
use engine_core::{Result, anyhow::anyhow};
use engine_desktop::{OffscreenSprites, RenderLimits, WgpuBackends};
use engine_project::{EditOperation, Runtime, SceneEntity, rendering3d::*};
use glam::{Mat4, Vec3};
use std::path::Path;

pub fn run(manifest: &Path, host: Box<dyn PlayHost>) -> Result<()> {
    let mut controller = EditorController::new(host);
    controller.open(manifest, false)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rust Game Engine",
        options,
        Box::new(move |_| Ok(Box::new(PortableEditor::new(controller)))),
    )
    .map_err(|error| anyhow!(error.to_string()))
}

#[derive(Clone, Debug)]
pub struct ViewNavigation {
    pub three_dimensional: bool,
    pub center: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub pixels_per_unit: f32,
}
impl Default for ViewNavigation {
    fn default() -> Self {
        Self {
            three_dimensional: false,
            center: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            distance: 8.0,
            pixels_per_unit: 16.0,
        }
    }
}
impl ViewNavigation {
    pub fn camera_world(&self) -> Mat4 {
        Mat4::from_translation(Vec3::from_array(self.center))
            * Mat4::from_rotation_y(self.yaw)
            * Mat4::from_rotation_x(self.pitch)
            * Mat4::from_translation(Vec3::Z * self.distance)
    }
    pub fn screen_delta(&self, delta: [f32; 2], height: f32) -> [f32; 3] {
        if self.three_dimensional {
            let camera = self.camera_world();
            let scale = self.distance * (std::f32::consts::FRAC_PI_6).tan() * 2.0 / height;
            (camera.x_axis.truncate() * delta[0] * scale
                - camera.y_axis.truncate() * delta[1] * scale)
                .to_array()
        } else {
            [
                delta[0] / self.pixels_per_unit,
                -delta[1] / self.pixels_per_unit,
                0.0,
            ]
        }
    }
}

struct DragEdit {
    entity: SceneEntity,
    revision: String,
    origin: egui::Pos2,
}
struct PortableEditor {
    controller: EditorController,
    navigation: ViewNavigation,
    runtime: Option<Runtime>,
    preview_camera: Option<engine_core::bevy_ecs::entity::Entity>,
    revision: String,
    frame: Option<SceneRenderFrame>,
    renderer: Option<OffscreenSprites>,
    image: Option<egui::TextureHandle>,
    dirty_preview: bool,
    error: String,
    inspected: String,
    entity_json: String,
    drag: Option<DragEdit>,
    confirm_close: bool,
    close_approved: bool,
    viewport_rect: Option<egui::Rect>,
}
impl PortableEditor {
    fn new(controller: EditorController) -> Self {
        Self {
            controller,
            navigation: Default::default(),
            runtime: None,
            preview_camera: None,
            revision: String::new(),
            frame: None,
            renderer: None,
            image: None,
            dirty_preview: true,
            error: String::new(),
            inspected: String::new(),
            entity_json: String::new(),
            drag: None,
            confirm_close: false,
            close_approved: false,
            viewport_rect: None,
        }
    }
    fn report(&mut self, result: Result<()>) {
        match result {
            Ok(()) => {
                self.error.clear();
                self.dirty_preview = true;
            }
            Err(error) => self.error = format!("{error:#}"),
        }
    }
    fn selected(&self) -> Option<SceneEntity> {
        let id = self.controller.selected()?;
        self.controller
            .document()?
            .snapshot()
            .entities
            .iter()
            .find(|e| e.id == id)
            .cloned()
    }
    fn apply_entity(&mut self, entity: SceneEntity) {
        let revision = self
            .controller
            .document()
            .map(|d| d.revision().to_owned())
            .unwrap_or_default();
        let result = self
            .controller
            .apply(EditOperation::ReplaceEntity { entity }, &revision);
        self.report(result);
    }
    fn rebuild(&mut self, ctx: &egui::Context) -> Result<()> {
        let document = self
            .controller
            .document()
            .ok_or_else(|| anyhow!("No scene open"))?;
        let revision = document.revision().to_owned();
        if self.revision != revision || self.runtime.is_none() {
            let mut project = document.project().clone();
            project.scene = document.snapshot().clone();
            self.runtime = Some(Runtime::new(project)?);
            self.preview_camera = None;
            self.revision = revision;
        }
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| anyhow!("No preview runtime"))?;
        if let Some(entity) = self.preview_camera.take() {
            runtime.app_mut().world_mut().despawn(entity);
        }
        // Preview cameras are transient ECS state and are never written into the document.
        let cameras: Vec<_> = runtime
            .app()
            .world()
            .iter_entities()
            .filter(|e| e.contains::<Camera3D>())
            .map(|e| e.id())
            .collect();
        for entity in cameras {
            runtime
                .app_mut()
                .world_mut()
                .entity_mut(entity)
                .remove::<Camera3D>();
        }
        if self.navigation.three_dimensional {
            self.preview_camera = Some(
                runtime
                    .app_mut()
                    .world_mut()
                    .spawn((Camera3D::default(), engine_project::Transform::default()))
                    .id(),
            );
        }
        let mut frame = runtime.extract_scene_frame()?;
        frame.sprites.camera.logical_size = [640, 360];
        frame.sprites.camera.center = [self.navigation.center[0], self.navigation.center[1]];
        frame.sprites.camera.pixels_per_unit = self.navigation.pixels_per_unit;
        frame.sprites.camera.pixel_snap = false;
        if self.navigation.three_dimensional {
            frame.camera3d = Some(CameraInstance3D {
                camera: Camera3D::default(),
                world: self.navigation.camera_world().to_cols_array_2d(),
            });
            if frame.lights.is_empty() {
                // Unlit scenes preview with a default directional light.
                frame
                    .lights
                    .push(engine_project::lights::LightInstance3D::directional_toward(
                        "editor:preview-light",
                        [0.4, 0.7, 1.0],
                        Default::default(),
                    )?);
            }
        }
        if self.renderer.is_none() {
            self.renderer = Some(OffscreenSprites::wgpu(
                WgpuBackends::Primary,
                640,
                360,
                RenderLimits::default(),
            )?);
        }
        let renderer = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("No preview renderer"))?;
        renderer.render_scene(&frame)?;
        let image = egui::ColorImage::from_rgba_unmultiplied([640, 360], &renderer.read_rgba()?);
        if let Some(texture) = &mut self.image {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.image =
                Some(ctx.load_texture("scene preview", image, egui::TextureOptions::LINEAR));
        }
        self.frame = Some(frame);
        self.dirty_preview = false;
        Ok(())
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Scene editor");
            if ui.button("Save").clicked() {
                let r = self.controller.save();
                self.report(r);
            }
            if ui.button("Undo").clicked() {
                let r = self.controller.undo();
                self.report(r);
            }
            if ui.button("Redo").clicked() {
                let r = self.controller.redo();
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
            if self.controller.is_dirty() {
                ui.label("Unsaved");
            }
        });
    }
    fn hierarchy(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hierarchy");
        let entities = self
            .controller
            .document()
            .map(|d| d.snapshot().entities.clone())
            .unwrap_or_default();
        for entity in entities {
            if ui
                .selectable_label(self.controller.selected() == Some(&entity.id), &entity.name)
                .clicked()
            {
                let r = self.controller.select(Some(entity.id));
                self.report(r);
            }
        }
        ui.separator();
        ui.heading("Assets");
        if let Some(document) = self.controller.document() {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for asset in document.project().assets.values() {
                    ui.label(format!("{} · {}", asset.kind, asset.id));
                }
            });
        }
    }
    fn inspector(&mut self, ui: &mut egui::Ui) {
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
    fn viewport(&mut self, ui: &mut egui::Ui) {
        ui.label("Right drag: orbit · Middle drag: pan · Wheel: zoom · Left drag: move selection");
        if self.dirty_preview {
            let result = self.rebuild(ui.ctx());
            if let Err(error) = result {
                self.error = format!("{error:#}");
                self.dirty_preview = false;
            }
        }
        let Some(image) = &self.image else {
            ui.label("No rendered preview");
            return;
        };
        let width = ui.available_width().max(1.0);
        let height = (width * 360.0 / 640.0).min(ui.available_height()).max(1.0);
        let size = egui::vec2(height * 640.0 / 360.0, height);
        let response =
            ui.add(egui::Image::new((image.id(), size)).sense(egui::Sense::click_and_drag()));
        let rect = response.rect;
        self.viewport_rect = Some(rect);
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.navigation.distance =
                    (self.navigation.distance * (-scroll * 0.005).exp()).clamp(0.05, 10000.0);
                self.navigation.pixels_per_unit =
                    (self.navigation.pixels_per_unit * (scroll * 0.005).exp()).clamp(0.05, 4096.0);
                self.dirty_preview = true;
            }
        }
        let delta = ui.input(|i| i.pointer.delta());
        if response.dragged_by(egui::PointerButton::Secondary) {
            self.navigation.yaw -= delta.x * 0.01;
            self.navigation.pitch = (self.navigation.pitch - delta.y * 0.01).clamp(-1.5, 1.5);
            self.dirty_preview = true;
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            let shift = self.navigation.screen_delta(
                [
                    -delta.x * 640.0 / rect.width(),
                    -delta.y * 360.0 / rect.height(),
                ],
                360.0,
            );
            for (value, delta) in self.navigation.center.iter_mut().zip(shift) {
                *value += delta;
            }
            self.dirty_preview = true;
        }
        if response.clicked()
            && let Some(pointer) = response.interact_pointer_pos()
            && let Some(id) = self.pick(pointer, rect)
        {
            let r = self.controller.select(Some(id));
            self.report(r);
        }
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(origin) = response.interact_pointer_pos()
            && let Some(entity) = self.selected()
        {
            self.drag = Some(DragEdit {
                entity,
                origin,
                revision: self
                    .controller
                    .document()
                    .map(|d| d.revision().into())
                    .unwrap_or_default(),
            });
        }
        if response.drag_stopped_by(egui::PointerButton::Primary)
            && let Some(drag) = self.drag.take()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let result = self
                .translated_drag(&drag, pointer, rect)
                .and_then(|entity| {
                    self.controller
                        .apply(EditOperation::ReplaceEntity { entity }, &drag.revision)
                });
            // Reconstruct from the accepted document even when a stale gesture fails.
            self.runtime = None;
            self.dirty_preview = true;
            self.report(result);
        } else if response.dragged_by(egui::PointerButton::Primary)
            && let Some(drag) = &self.drag
            && let Some(pointer) = response.interact_pointer_pos()
        {
            match self.translated_drag(drag, pointer, rect) {
                Ok(entity) => {
                    if let Some(runtime) = &mut self.runtime
                        && let Some(id) = runtime.entity(&entity.id)
                        && let Some(transform) = entity.components.transform
                    {
                        runtime
                            .app_mut()
                            .world_mut()
                            .entity_mut(id)
                            .insert(transform);
                        self.dirty_preview = true;
                    }
                }
                Err(error) => self.error = error.to_string(),
            }
        }
        if let Some(entity) = self.selected()
            && let Some(position) = self
                .world_position(&entity)
                .and_then(|p| self.project(p, rect))
        {
            let painter = ui.painter();
            painter.circle_stroke(position, 9.0, egui::Stroke::new(2.0, egui::Color32::YELLOW));
            painter.line_segment(
                [position, position + egui::vec2(32.0, 0.0)],
                egui::Stroke::new(2.0, egui::Color32::RED),
            );
            painter.line_segment(
                [position, position - egui::vec2(0.0, 32.0)],
                egui::Stroke::new(2.0, egui::Color32::GREEN),
            );
        }
    }
    fn translated_drag(
        &self,
        drag: &DragEdit,
        pointer: egui::Pos2,
        rect: egui::Rect,
    ) -> Result<SceneEntity> {
        let parent_inverse = self.parent_inverse(&drag.entity)?;
        let mut entity = drag.entity.clone();
        let transform = entity
            .components
            .transform
            .as_mut()
            .ok_or_else(|| anyhow!("Selected entity has no transform"))?;
        let delta = pointer - drag.origin;
        let shift = self.navigation.screen_delta(
            [
                delta.x * 640.0 / rect.width(),
                delta.y * 360.0 / rect.height(),
            ],
            360.0,
        );
        let shift = parent_inverse
            .transform_vector3(Vec3::from_array(shift))
            .to_array();
        for (value, delta) in transform.position.iter_mut().zip(shift) {
            *value += delta;
        }
        Ok(entity)
    }
    fn project(&self, position: [f32; 3], rect: egui::Rect) -> Option<egui::Pos2> {
        let point = if self.navigation.three_dimensional {
            let camera = CameraInstance3D {
                camera: Camera3D::default(),
                world: self.navigation.camera_world().to_cols_array_2d(),
            };
            let clip = engine_desktop::camera_matrix(&camera, 640.0 / 360.0).ok()?
                * Vec3::from_array(position).extend(1.0);
            if clip.w <= 0.0 {
                return None;
            }
            [clip.x / clip.w, clip.y / clip.w]
        } else {
            [
                (position[0] - self.navigation.center[0]) * self.navigation.pixels_per_unit / 320.0,
                (position[1] - self.navigation.center[1]) * self.navigation.pixels_per_unit / 180.0,
            ]
        };
        Some(egui::pos2(
            rect.center().x + point[0] * rect.width() * 0.5,
            rect.center().y - point[1] * rect.height() * 0.5,
        ))
    }
    fn world_position(&self, entity: &SceneEntity) -> Option<[f32; 3]> {
        let world = self.runtime.as_ref()?.app().world();
        let id = engine_project::runtime::find_entity(world, &entity.id)?;
        engine_project::world_transform(world, id)
            .ok()
            .map(|transform| transform.position())
    }
    fn parent_inverse(&self, entity: &SceneEntity) -> Result<Mat4> {
        let Some(parent) = &entity.parent else {
            return Ok(Mat4::IDENTITY);
        };
        let world = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.app().world())
            .ok_or_else(|| anyhow!("Preview world unavailable"))?;
        let id = engine_project::runtime::find_entity(world, parent)
            .ok_or_else(|| anyhow!("Parent entity unavailable"))?;
        let transform = engine_project::world_transform(world, id)?;
        let matrix = Mat4::from_cols_array_2d(&transform.matrix);
        if !matrix.is_finite() || matrix.determinant().abs() <= 1e-8 {
            return Err(anyhow!("Parent transform is singular"));
        }
        Ok(matrix.inverse())
    }
    fn pick(&self, pointer: egui::Pos2, rect: egui::Rect) -> Option<String> {
        self.controller
            .document()?
            .snapshot()
            .entities
            .iter()
            .filter_map(|entity| {
                let position = self.project(self.world_position(entity)?, rect)?;
                let distance = position.distance(pointer);
                (distance < 24.0).then_some((distance, entity.id.clone()))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }
}
impl eframe::App for PortableEditor {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Err(error) = self.controller.poll() {
            self.error = error.to_string();
        }
        if self
            .controller
            .document()
            .is_some_and(|d| d.revision() != self.revision)
        {
            self.dirty_preview = true;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_approved {
            if self.controller.is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_close = true;
            } else if let Err(error) = self.controller.close(false) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.error = error.to_string();
            }
        }
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("console")
            .min_size(90.0)
            .show(ui, |ui| {
                ui.label(self.controller.status());
                if !self.error.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
                }
                egui::ScrollArea::vertical()
                    .max_height(70.0)
                    .show(ui, |ui| {
                        for line in self.controller.log().iter().rev().take(8) {
                            ui.label(line);
                        }
                    });
            });
        egui::Panel::left("hierarchy")
            .default_size(210.0)
            .show(ui, |ui| self.hierarchy(ui));
        egui::Panel::right("inspector")
            .default_size(300.0)
            .show(ui, |ui| self.inspector(ui));
        egui::CentralPanel::default().show(ui, |ui| self.viewport(ui));
        if self.confirm_close {
            egui::Window::new("Unsaved scene")
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label("Save changes before closing?");
                    ui.horizontal(|ui| {
                        if ui.button("Save and close").clicked() {
                            let r = self
                                .controller
                                .save()
                                .and_then(|()| self.controller.close(false));
                            let success = r.is_ok();
                            self.report(r);
                            if success {
                                self.close_approved = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                self.confirm_close = false;
                            }
                        }
                        if ui.button("Discard and close").clicked() {
                            let r = self.controller.close(true);
                            let success = r.is_ok();
                            self.report(r);
                            if success {
                                self.close_approved = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                self.confirm_close = false;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_close = false;
                        }
                    });
                });
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Host;
    impl PlayHost for Host {
        fn start(&mut self, _: engine_project::Project) -> Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
        fn poll(&mut self) -> Result<crate::PlayStatus> {
            Ok(crate::PlayStatus::Running)
        }
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scene.json"),r#"{"schema":1,"scene_id":"scene","entities":[{"id":"object","name":"Object","components":{"Transform":{"position":[0,0,0]}}}]}"#).unwrap();
        std::fs::write(
            dir.path().join("project.json"),
            r#"{"schema":1,"name":"Editor","startup_scene":"scene.json","assets":[]}"#,
        )
        .unwrap();
        dir
    }
    fn pass(app: &mut PortableEditor, ctx: &egui::Context, events: Vec<egui::Event>) {
        use eframe::App;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
        // This input test has no egui GPU backend to consume texture updates.
        output.textures_delta.clear();
    }
    #[test]
    fn real_egui_drag_commits_once_and_undo_restores_the_scene() {
        let dir = fixture();
        let mut controller = EditorController::new(Box::new(Host));
        controller
            .open(dir.path().join("project.json"), false)
            .unwrap();
        let mut app = PortableEditor::new(controller);
        app.renderer = Some(OffscreenSprites::headless(640, 360, RenderLimits::default()).unwrap());
        let ctx = egui::Context::default();
        pass(&mut app, &ctx, vec![]);
        pass(&mut app, &ctx, vec![]);
        assert!(app.image.is_some(), "{}", app.error);
        let center = app.viewport_rect.unwrap().center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        pass(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(center), button(center, true)],
        );
        pass(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(center + egui::vec2(10.0, 0.0))],
        );
        pass(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(center + egui::vec2(80.0, 0.0))],
        );
        assert_eq!(
            app.selected()
                .unwrap()
                .components
                .transform
                .unwrap()
                .position,
            [0.0; 3],
            "drag previews must not mutate authored state"
        );
        assert!(
            app.world_position(&app.selected().unwrap()).unwrap()[0] > 0.5,
            "the preview world must move during the held gesture"
        );
        pass(
            &mut app,
            &ctx,
            vec![button(center + egui::vec2(80.0, 0.0), false)],
        );
        assert!(
            app.selected()
                .unwrap()
                .components
                .transform
                .unwrap()
                .position[0]
                > 0.5,
            "{}",
            app.error
        );
        app.controller.undo().unwrap();
        assert_eq!(
            app.selected()
                .unwrap()
                .components
                .transform
                .unwrap()
                .position,
            [0.0; 3]
        );
        assert!(
            !app.controller.is_dirty(),
            "one gesture must produce one history entry"
        );
    }
    #[test]
    fn navigation_maps_screen_motion_to_world_axes() {
        let view = ViewNavigation::default();
        assert_eq!(view.screen_delta([16.0, -16.0], 360.0), [1.0, 1.0, 0.0]);
        let view = ViewNavigation {
            three_dimensional: true,
            yaw: std::f32::consts::FRAC_PI_2,
            ..Default::default()
        };
        let movement = view.screen_delta([100.0, 0.0], 360.0);
        assert!(movement[0].abs() < 1e-5 && movement[2] < -1.0);
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: view.camera_world().to_cols_array_2d(),
        };
        let projected = engine_desktop::camera_matrix(&camera, 16.0 / 9.0).unwrap()
            * Vec3::from_array(view.center).extend(1.0);
        assert!(projected.x.abs() < 1e-5 && projected.y.abs() < 1e-5 && projected.w > 0.0);
    }
}
