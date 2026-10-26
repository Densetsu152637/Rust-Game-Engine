use super::*;
use engine_editor_settings::GizmoPivot;

#[derive(Clone, Copy)]
pub(super) enum Action {
    Replace,
    Add,
    Toggle,
}
pub(super) fn action_for(modifiers: egui::Modifiers) -> Action {
    if modifiers.command || modifiers.ctrl {
        Action::Toggle
    } else if modifiers.shift {
        Action::Add
    } else {
        Action::Replace
    }
}
pub(super) fn apply_click(
    controller: &mut EditorController,
    id: Option<String>,
    action: Action,
) -> Result<()> {
    match (action, id) {
        (Action::Replace, id) => controller.select(id),
        (Action::Add, Some(id)) => controller.extend_selection(&id),
        (Action::Toggle, Some(id)) => controller.toggle_selection(&id),
        _ => Ok(()),
    }
}
pub(super) fn apply_box(
    controller: &mut EditorController,
    ids: Vec<String>,
    action: Action,
) -> Result<()> {
    let mut selected = match action {
        Action::Replace => vec![],
        _ => controller.selection().to_vec(),
    };
    for id in ids {
        if matches!(action, Action::Toggle) && selected.contains(&id) {
            selected.retain(|s| s != &id);
        } else if !selected.contains(&id) {
            selected.push(id);
        }
    }
    controller.select_many(selected)
}
pub(super) struct Marquee {
    pub origin: egui::Pos2,
    pub current: egui::Pos2,
}
impl Marquee {
    pub fn rect(&self) -> egui::Rect {
        egui::Rect::from_two_pos(self.origin, self.current)
    }
}
impl PortableEditor {
    pub(super) fn world_position_by_id(&self, id: &str) -> Option<[f32; 3]> {
        let runtime = self.runtime.as_ref()?;
        engine_project::world_transform(runtime.app().world(), runtime.entity(id)?)
            .ok()
            .map(|t| t.position())
    }
    pub(super) fn gizmo_setup(&self) -> Option<(gizmo::Basis, Vec<gizmo::EntityStart>)> {
        let document = self.controller.document()?;
        let runtime = self.runtime.as_ref()?;
        let entities = &document.snapshot().entities;
        let matrix = |id: &str| {
            engine_project::world_transform(runtime.app().world(), runtime.entity(id)?)
                .ok()
                .map(|t| Mat4::from_cols_array_2d(&t.matrix))
        };
        let active_world = matrix(self.controller.selected()?)?;
        let mut starts = Vec::new();
        let mut pivot = Vec3::ZERO;
        let mut count = 0;
        for id in self.controller.selection() {
            let entity = entities.iter().find(|e| &e.id == id)?;
            let world = matrix(id)?;
            pivot += world.w_axis.truncate();
            count += 1;
            // A selected descendant follows its ancestor; applying twice would double its motion.
            let mut parent = entity.parent.as_deref();
            let mut selected_ancestor = false;
            while let Some(id) = parent {
                if self.controller.is_selected(id) {
                    selected_ancestor = true;
                    break;
                }
                parent = entities.iter().find(|e| e.id == id)?.parent.as_deref();
            }
            if selected_ancestor {
                continue;
            }
            let parent_world = match entity.parent.as_deref() {
                Some(parent) => matrix(parent)?,
                None => Mat4::IDENTITY,
            };
            if !parent_world.is_finite() || parent_world.determinant().abs() <= 1e-8 {
                return None;
            }
            starts.push(gizmo::EntityStart {
                id: id.clone(),
                local: entity.components.transform.clone()?,
                world,
                parent_world,
            });
        }
        if starts.is_empty() {
            return None;
        }
        pivot = if self.settings.gizmo.pivot == GizmoPivot::Active {
            active_world.w_axis.truncate()
        } else {
            pivot / count as f32
        };
        Some((
            gizmo::Basis::new(self.settings.gizmo.space, pivot, Some(&active_world)),
            starts,
        ))
    }
    pub(super) fn gizmo_hit(
        &self,
        view: &viewport::ViewMath,
        pointer: egui::Pos2,
    ) -> Option<gizmo::Handle> {
        let (basis, _) = self.gizmo_setup()?;
        gizmo::hit_test(self.settings.gizmo.mode, &basis, view, pointer)
    }
    pub(super) fn pick(&self, pointer: egui::Pos2, rect: egui::Rect) -> Option<String> {
        let (x, y) = self.view_math(rect)?.buffer_pixel(pointer)?;
        self.picker.at(x, y).map(str::to_owned)
    }
    pub(super) fn box_members(&self, area: egui::Rect, rect: egui::Rect) -> Vec<String> {
        self.picker.members(area, rect)
    }
    pub(super) fn cancel_interaction(&mut self) {
        if !matches!(self.interaction, Interaction::None) {
            self.interaction = Interaction::None;
            self.runtime = None;
            self.dirty_preview = true;
        }
    }
}
