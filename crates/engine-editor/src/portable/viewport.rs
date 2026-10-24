//! Scene-view camera, projection math, grid overlay and viewport interaction.
use super::{
    PortableEditor,
    gizmo::{self, Delta, Gesture},
    selection::{self, Marquee},
};
use eframe::egui;
use engine_editor_settings::CameraBookmark;
use engine_project::{EditOperation, rendering3d::*};
use glam::{Mat4, Vec2, Vec3, Vec4};

/// Resolution of the offscreen preview and of the picking ID buffer.
pub const PREVIEW_WIDTH: u32 = 640;
pub const PREVIEW_HEIGHT: u32 = 360;

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
    /// Captures this view as a named bookmark.
    pub fn to_bookmark(&self, name: &str) -> CameraBookmark {
        CameraBookmark {
            name: name.to_owned(),
            three_dimensional: self.three_dimensional,
            center: self.center,
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
            pixels_per_unit: self.pixels_per_unit,
        }
    }
    /// Restores a bookmarked view.
    pub fn apply_bookmark(&mut self, bookmark: &CameraBookmark) {
        self.three_dimensional = bookmark.three_dimensional;
        self.center = bookmark.center;
        self.yaw = bookmark.yaw;
        self.pitch = bookmark.pitch;
        self.distance = bookmark.distance;
        self.pixels_per_unit = bookmark.pixels_per_unit;
    }
}

/// The world-to-clip matrix the preview and the ID buffer both use (depth 0..1).
pub fn view_projection(nav: &ViewNavigation) -> Option<Mat4> {
    if nav.three_dimensional {
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: nav.camera_world().to_cols_array_2d(),
        };
        engine_desktop::camera_matrix(&camera, PREVIEW_WIDTH as f32 / PREVIEW_HEIGHT as f32).ok()
    } else {
        let sx = nav.pixels_per_unit / (PREVIEW_WIDTH as f32 / 2.0);
        let sy = nav.pixels_per_unit / (PREVIEW_HEIGHT as f32 / 2.0);
        // Larger world Z (and layer) is nearer: depth = 0.5 - z * 0.0005.
        let matrix = Mat4::from_cols(
            Vec4::new(sx, 0.0, 0.0, 0.0),
            Vec4::new(0.0, sy, 0.0, 0.0),
            Vec4::new(0.0, 0.0, -0.0005, 0.0),
            Vec4::new(-nav.center[0] * sx, -nav.center[1] * sy, 0.5, 1.0),
        );
        matrix.is_finite().then_some(matrix)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

/// Projection between world space and a screen rectangle showing the preview.
#[derive(Clone, Copy, Debug)]
pub struct ViewMath {
    pub view_proj: Mat4,
    inverse: Mat4,
    pub rect: egui::Rect,
    pub three_dimensional: bool,
    origin: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}
impl ViewMath {
    pub fn new(nav: &ViewNavigation, rect: egui::Rect) -> Option<Self> {
        let view_proj = view_projection(nav)?;
        if view_proj.determinant().abs() < 1e-20 {
            return None;
        }
        let inverse = view_proj.inverse();
        if !inverse.is_finite() {
            return None;
        }
        let (origin, right, up) = if nav.three_dimensional {
            let camera = nav.camera_world();
            (
                camera.w_axis.truncate(),
                camera.x_axis.truncate().normalize_or(Vec3::X),
                camera.y_axis.truncate().normalize_or(Vec3::Y),
            )
        } else {
            (Vec3::ZERO, Vec3::X, Vec3::Y)
        };
        Some(Self {
            view_proj,
            inverse,
            rect,
            three_dimensional: nav.three_dimensional,
            origin,
            right,
            up,
        })
    }
    fn to_screen(&self, ndc_x: f32, ndc_y: f32) -> egui::Pos2 {
        egui::pos2(
            self.rect.center().x + ndc_x * self.rect.width() * 0.5,
            self.rect.center().y - ndc_y * self.rect.height() * 0.5,
        )
    }
    pub fn clip(&self, point: Vec3) -> Vec4 {
        self.view_proj * point.extend(1.0)
    }
    pub fn project(&self, point: Vec3) -> Option<egui::Pos2> {
        let clip = self.clip(point);
        if !clip.is_finite() || clip.w <= 1e-6 {
            return None;
        }
        Some(self.to_screen(clip.x / clip.w, clip.y / clip.w))
    }
    /// Projects a segment, clipping it to the camera's front half-space.
    pub fn project_segment(&self, a: Vec3, b: Vec3) -> Option<[egui::Pos2; 2]> {
        const EPS: f32 = 1e-3;
        let (mut ca, mut cb) = (self.clip(a), self.clip(b));
        if !ca.is_finite() || !cb.is_finite() || (ca.w <= EPS && cb.w <= EPS) {
            return None;
        }
        if ca.w <= EPS {
            ca = ca.lerp(cb, (EPS - ca.w) / (cb.w - ca.w));
        } else if cb.w <= EPS {
            cb = cb.lerp(ca, (EPS - cb.w) / (ca.w - cb.w));
        }
        Some([
            self.to_screen(ca.x / ca.w, ca.y / ca.w),
            self.to_screen(cb.x / cb.w, cb.y / cb.w),
        ])
    }
    /// World-space ray through a screen position.
    pub fn ray(&self, pos: egui::Pos2) -> Ray {
        let ndc_x = (pos.x - self.rect.center().x) / (self.rect.width() * 0.5);
        let ndc_y = -(pos.y - self.rect.center().y) / (self.rect.height() * 0.5);
        let near = self.inverse * Vec4::new(ndc_x, ndc_y, 0.5, 1.0);
        let point = near.truncate() / near.w;
        if self.three_dimensional {
            Ray {
                origin: self.origin,
                dir: (point - self.origin).normalize_or(Vec3::NEG_Z),
            }
        } else {
            Ray {
                origin: point + Vec3::Z * 1000.0,
                dir: Vec3::NEG_Z,
            }
        }
    }
    /// World units covered by one screen pixel at `at`, measured along the camera right axis.
    pub fn world_per_pixel(&self, at: Vec3) -> f32 {
        match (self.project(at), self.project(at + self.right)) {
            (Some(a), Some(b)) if a.distance(b) > 1e-4 => 1.0 / a.distance(b),
            _ => 0.01,
        }
    }
    /// A screen-space motion converted to a world vector in the camera plane at `at`.
    pub fn screen_to_world(&self, delta: Vec2, at: Vec3) -> Vec3 {
        let scale = self.world_per_pixel(at);
        self.right * delta.x * scale - self.up * delta.y * scale
    }
    /// Maps a screen position to ID-buffer pixel coordinates.
    pub fn buffer_pixel(&self, pos: egui::Pos2) -> Option<(u32, u32)> {
        if !self.rect.contains(pos) {
            return None;
        }
        let x = (pos.x - self.rect.min.x) / self.rect.width() * PREVIEW_WIDTH as f32;
        let y = (pos.y - self.rect.min.y) / self.rect.height() * PREVIEW_HEIGHT as f32;
        Some((
            (x as u32).min(PREVIEW_WIDTH - 1),
            (y as u32).min(PREVIEW_HEIGHT - 1),
        ))
    }
}

/// Draws the configurable ground grid (XZ plane in 3D, XY plane in 2D).
pub fn draw_grid(painter: &egui::Painter, view: &ViewMath, center: Vec3, spacing: f32) {
    if !(spacing.is_finite() && spacing > 0.0) {
        return;
    }
    let pixel = view.world_per_pixel(center);
    let mut spacing = spacing;
    // Coarsen by powers of two until lines are at least 8 px apart.
    while spacing / pixel < 8.0 && spacing < 1.0e6 {
        spacing *= 2.0;
    }
    let (reach, plane_axis) = if view.three_dimensional {
        (20.0 * spacing, 2)
    } else {
        let half = view.rect.width().max(view.rect.height()) * 0.5 * pixel;
        (half.min(200.0 * spacing), 2)
    };
    let steps = (reach / spacing).ceil().clamp(1.0, 200.0) as i32;
    let snap = |value: f32| (value / spacing).round() * spacing;
    let origin = Vec3::new(snap(center.x), snap(center.y), snap(center.z));
    let _ = plane_axis;
    // 3D lies in XZ at y = 0; 2D lies in XY at z = 0.
    let (a_axis, b_axis) = if view.three_dimensional {
        (Vec3::X, Vec3::Z)
    } else {
        (Vec3::X, Vec3::Y)
    };
    let base = if view.three_dimensional {
        Vec3::new(origin.x, 0.0, origin.z)
    } else {
        Vec3::new(origin.x, origin.y, 0.0)
    };
    let extent = steps as f32 * spacing;
    for i in -steps..=steps {
        let offset = i as f32 * spacing;
        for (along, across) in [(a_axis, b_axis), (b_axis, a_axis)] {
            let line_origin = base + across * offset;
            let world_offset = (line_origin * across).length();
            let axis_line = world_offset < spacing * 0.01;
            let color = if axis_line {
                if along == Vec3::X {
                    egui::Color32::from_rgba_unmultiplied(220, 70, 70, 150)
                } else if along == Vec3::Y {
                    egui::Color32::from_rgba_unmultiplied(90, 200, 90, 150)
                } else {
                    egui::Color32::from_rgba_unmultiplied(80, 120, 230, 150)
                }
            } else {
                egui::Color32::from_rgba_unmultiplied(200, 200, 200, 38)
            };
            if let Some([p, q]) = view.project_segment(
                line_origin - along * extent,
                line_origin + along * extent,
            ) {
                painter.line_segment([p, q], egui::Stroke::new(1.0, color));
            }
        }
    }
}

impl PortableEditor {
    pub(super) fn view_math(&self, rect: egui::Rect) -> Option<ViewMath> {
        ViewMath::new(&self.navigation, rect)
    }

    pub(super) fn project(&self, position: [f32; 3], rect: egui::Rect) -> Option<egui::Pos2> {
        self.view_math(rect)?.project(Vec3::from_array(position))
    }

    pub(super) fn viewport(&mut self, ui: &mut egui::Ui) {
        ui.label(
            "Right drag: orbit · Middle drag: pan · Wheel: zoom · Left click: select (Ctrl toggle, Shift add) · Left drag: gizmo handle or box select · W/E/R: move/rotate/scale",
        );
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
        let aspect = PREVIEW_WIDTH as f32 / PREVIEW_HEIGHT as f32;
        let width = ui.available_width().max(1.0);
        let height = (width / aspect).min(ui.available_height()).max(1.0);
        let size = egui::vec2(height * aspect, height);
        let response =
            ui.add(egui::Image::new((image.id(), size)).sense(egui::Sense::click_and_drag()));
        let rect = response.rect;
        self.viewport_rect = Some(rect);
        self.viewport_shortcuts(ui, &response);
        self.navigate(ui, &response, rect);
        self.interact(ui, &response, rect);
        self.paint_overlay(ui, rect);
    }

    fn viewport_shortcuts(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if !response.hovered() || ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        use engine_editor_settings::GizmoMode;
        let (w, e, r) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::W),
                i.key_pressed(egui::Key::E),
                i.key_pressed(egui::Key::R),
            )
        });
        for (pressed, mode) in [
            (w, GizmoMode::Translate),
            (e, GizmoMode::Rotate),
            (r, GizmoMode::Scale),
        ] {
            if pressed && self.settings.gizmo.mode != mode {
                self.settings.gizmo.mode = mode;
                self.settings_dirty = true;
            }
        }
    }

    fn navigate(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) {
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
                    -delta.x * PREVIEW_WIDTH as f32 / rect.width(),
                    -delta.y * PREVIEW_HEIGHT as f32 / rect.height(),
                ],
                PREVIEW_HEIGHT as f32,
            );
            for (value, delta) in self.navigation.center.iter_mut().zip(shift) {
                *value += delta;
            }
            self.dirty_preview = true;
        }
    }

    /// Primary-button click, gizmo gestures and box selection.
    fn interact(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect) {
        let modifiers = ui.input(|i| i.modifiers);
        let action = selection::action_for(modifiers);
        let Some(view) = self.view_math(rect) else {
            return;
        };
        if response.clicked_by(egui::PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let picked = self.pick(pointer, rect);
            let result = selection::apply_click(&mut self.controller, picked, action);
            self.report(result);
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            let press = ui
                .input(|i| i.pointer.press_origin())
                .or(response.interact_pointer_pos());
            if let Some(press) = press {
                self.begin_primary_drag(&view, press);
            }
        }
        let pointer = response.interact_pointer_pos();
        if response.dragged_by(egui::PointerButton::Primary)
            && let Some(pointer) = pointer
        {
            match &mut self.interaction {
                Interaction::Gizmo(_) => self.update_gizmo_drag(&view, pointer),
                Interaction::Marquee(marquee) => marquee.current = pointer,
                Interaction::None => {}
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            match std::mem::take(&mut self.interaction) {
                Interaction::Gizmo(drag) => self.finish_gizmo_drag(drag, &view, pointer),
                Interaction::Marquee(mut marquee) => {
                    if let Some(pointer) = pointer {
                        marquee.current = pointer;
                    }
                    self.finish_marquee(&marquee, rect, action);
                }
                Interaction::None => {}
            }
        }
        if response.hovered() && matches!(self.interaction, Interaction::None) {
            self.hover_handle = ui
                .input(|i| i.pointer.hover_pos())
                .and_then(|pos| self.gizmo_hit(&view, pos));
        }
    }

    fn begin_primary_drag(&mut self, view: &ViewMath, press: egui::Pos2) {
        if let Some(handle) = self.gizmo_hit(view, press)
            && let Some((basis, starts)) = self.gizmo_setup()
        {
            let ray = view.ray(press);
            let revision = self
                .controller
                .document()
                .map(|d| d.revision().to_owned())
                .unwrap_or_default();
            if let Some(gesture) = Gesture::begin(
                handle,
                self.settings.gizmo.mode,
                basis,
                &ray,
                press,
                view,
            ) {
                self.interaction = Interaction::Gizmo(GizmoDrag {
                    gesture,
                    starts,
                    revision,
                    last: None,
                });
                return;
            }
        }
        self.interaction = Interaction::Marquee(Marquee {
            origin: press,
            current: press,
        });
    }

    fn update_gizmo_drag(&mut self, view: &ViewMath, pointer: egui::Pos2) {
        let snapping = self.settings.snapping.clone();
        let ray = view.ray(pointer);
        let Interaction::Gizmo(drag) = &mut self.interaction else {
            return;
        };
        let Some(delta) = drag.gesture.delta(&ray, pointer, view, &snapping) else {
            return;
        };
        let transforms = gizmo::apply(
            &delta,
            drag.gesture.basis.pivot,
            &drag.starts,
            view.three_dimensional,
        );
        let ids: Vec<_> = drag.starts.iter().map(|s| s.id.clone()).collect();
        drag.last = Some(delta);
        if let Some(runtime) = &mut self.runtime {
            for (id, transform) in ids.iter().zip(transforms) {
                if let Some(entity) = runtime.entity(id) {
                    runtime
                        .app_mut()
                        .world_mut()
                        .entity_mut(entity)
                        .insert(transform);
                }
            }
            self.dirty_preview = true;
        }
    }

    fn finish_gizmo_drag(&mut self, drag: GizmoDrag, view: &ViewMath, pointer: Option<egui::Pos2>) {
        let snapping = self.settings.snapping.clone();
        let mut drag = drag;
        let delta = pointer
            .and_then(|pointer| {
                drag.gesture
                    .delta(&view.ray(pointer), pointer, view, &snapping)
            })
            .or_else(|| drag.last.clone());
        let operation = delta.and_then(|delta| {
            let transforms = gizmo::apply(
                &delta,
                drag.gesture.basis.pivot,
                &drag.starts,
                view.three_dimensional,
            );
            self.edit_for_transforms(&drag.starts, transforms)
        });
        // Reconstruct from the accepted document even when a stale gesture fails.
        let result = match operation {
            Some(operation) => self.controller.apply(operation, &drag.revision),
            None => Ok(()),
        };
        self.runtime = None;
        self.dirty_preview = true;
        self.report(result);
    }

    /// One `ReplaceEntity` (single) or `Batch` (multi) for a whole gesture.
    fn edit_for_transforms(
        &self,
        starts: &[gizmo::EntityStart],
        transforms: Vec<engine_project::Transform>,
    ) -> Option<EditOperation> {
        let document = self.controller.document()?;
        let mut operations = Vec::new();
        for (start, transform) in starts.iter().zip(transforms) {
            if transform == start.local {
                continue;
            }
            let mut entity = document
                .snapshot()
                .entities
                .iter()
                .find(|e| e.id == start.id)?
                .clone();
            entity.components.transform = Some(transform);
            operations.push(EditOperation::ReplaceEntity { entity });
        }
        match operations.len() {
            0 => None,
            1 => operations.pop(),
            _ => Some(EditOperation::Batch { operations }),
        }
    }

    fn finish_marquee(&mut self, marquee: &Marquee, rect: egui::Rect, action: selection::Action) {
        if marquee.rect().width() < 4.0 && marquee.rect().height() < 4.0 {
            // A tiny drag is a click on empty space or a pick.
            let picked = self.pick(marquee.current, rect);
            let result = selection::apply_click(&mut self.controller, picked, action);
            self.report(result);
            return;
        }
        let members = self.box_members(marquee.rect(), rect);
        let result = selection::apply_box(&mut self.controller, members, action);
        self.report(result);
    }

    fn paint_overlay(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        let Some(view) = self.view_math(rect) else {
            return;
        };
        let painter = ui.painter().with_clip_rect(rect);
        if self.settings.grid.visible {
            draw_grid(
                &painter,
                &view,
                Vec3::from_array(self.navigation.center),
                self.settings.grid.spacing,
            );
        }
        // Selection outlines at every selected origin; the active one is brighter.
        let selection: Vec<String> = self.controller.selection().to_vec();
        for id in &selection {
            if let Some(position) = self
                .world_position_by_id(id)
                .and_then(|p| view.project(Vec3::from_array(p)))
            {
                let active = self.controller.selected() == Some(id.as_str());
                painter.circle_stroke(
                    position,
                    if active { 9.0 } else { 6.0 },
                    egui::Stroke::new(
                        2.0,
                        if active {
                            egui::Color32::YELLOW
                        } else {
                            egui::Color32::from_rgb(255, 170, 40)
                        },
                    ),
                );
            }
        }
        if let Some((basis, _)) = self.gizmo_setup() {
            let active = match &self.interaction {
                Interaction::Gizmo(drag) => Some(drag.gesture.handle),
                _ => None,
            };
            gizmo::draw(
                &painter,
                self.settings.gizmo.mode,
                &basis,
                &view,
                self.hover_handle,
                active,
            );
        }
        if let Interaction::Marquee(marquee) = &self.interaction {
            let rect = marquee.rect();
            painter.rect_filled(
                rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(80, 140, 255, 40),
            );
            painter.rect_stroke(
                rect,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 170, 255)),
                egui::StrokeKind::Inside,
            );
        }
    }
}

/// An in-flight gizmo gesture and the entity state it started from.
pub struct GizmoDrag {
    pub gesture: Gesture,
    pub starts: Vec<gizmo::EntityStart>,
    pub revision: String,
    pub last: Option<Delta>,
}

/// What the primary mouse button is currently doing in the viewport.
#[derive(Default)]
pub enum Interaction {
    #[default]
    None,
    Gizmo(GizmoDrag),
    Marquee(Marquee),
}
