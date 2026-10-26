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

mod gizmo;
mod panels;
mod picking;
mod selection;
mod viewport;
use engine_editor_settings::EditorSettings;
use viewport::Interaction;
pub use viewport::ViewNavigation;

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
    interaction: Interaction,
    hover_handle: Option<gizmo::Handle>,
    settings: EditorSettings,
    settings_dirty: bool,
    settings_valid: bool,
    bookmark_name: String,
    picker: picking::Picker,
    confirm_close: bool,
    close_approved: bool,
    viewport_rect: Option<egui::Rect>,
}
impl PortableEditor {
    fn new(controller: EditorController) -> Self {
        let loaded = controller
            .document()
            .map(|d| EditorSettings::load(&d.project().root));
        let (settings, error, settings_valid) = match loaded {
            Some(Ok(settings)) => (settings, String::new(), true),
            Some(Err(error)) => (EditorSettings::default(), error.to_string(), false),
            None => (EditorSettings::default(), String::new(), true),
        };
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
            error,
            inspected: String::new(),
            entity_json: String::new(),
            interaction: Interaction::None,
            hover_handle: None,
            settings,
            settings_dirty: false,
            settings_valid,
            bookmark_name: String::new(),
            picker: Default::default(),
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
        self.picker = Default::default();
        self.picker = picking::Picker::build(&frame, &self.navigation)?;
        self.frame = Some(frame);
        self.dirty_preview = false;
        Ok(())
    }
    fn world_position(&self, entity: &SceneEntity) -> Option<[f32; 3]> {
        let world = self.runtime.as_ref()?.app().world();
        let id = engine_project::runtime::find_entity(world, &entity.id)?;
        engine_project::world_transform(world, id)
            .ok()
            .map(|transform| transform.position())
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
mod tests;
