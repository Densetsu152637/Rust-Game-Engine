use super::*;
use crate::input::ActionFrame;
use bevy_ecs::prelude::Resource;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiInteraction {
    pub root: String,
    pub node: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UiWidgetEvent {
    pub root: String,
    pub node: String,
    pub name: String,
    pub action: String,
    pub value: Value,
}

/// Retains keyboard focus and slider pointer capture; widget values live in UiRoot.
#[derive(Clone, Debug, Default, Resource)]
pub struct GameUi {
    pub(crate) focus: Option<UiInteraction>,
    captured: Option<UiInteraction>,
}

struct Target {
    root: usize,
    node: usize,
    key: UiInteraction,
    bounds: UiBox,
}

impl GameUi {
    pub fn focus(&self) -> Option<&UiInteraction> {
        self.focus.as_ref()
    }

    pub fn clear(&mut self) {
        self.focus = None;
        self.captured = None;
    }

    /// Pause interaction for a modal panel or lost window focus. Focus is retained;
    /// pointer capture is cancelled, so a later release cannot activate a widget.
    pub fn suspend(&mut self) {
        self.captured = None;
    }

    /// Root IDs must be unique. Roots are painted/hit-tested by (layer, root ID).
    /// Action names are ui_click, ui_confirm, ui_next/ui_down, ui_previous/ui_up,
    /// and ui_left/ui_right. A click is a pressed-edge activation; sliders capture
    /// its following held frames until release.
    pub fn tick(
        &mut self,
        roots: &mut [(String, UiRoot)],
        viewport: [u32; 2],
        input: &ActionFrame,
    ) -> Result<Vec<UiWidgetEvent>> {
        let targets = targets(roots, viewport)?;
        if self
            .focus
            .as_ref()
            .is_some_and(|key| !targets.iter().any(|t| &t.key == key))
        {
            self.focus = None;
        }
        if self
            .captured
            .as_ref()
            .is_some_and(|key| !targets.iter().any(|t| &t.key == key))
        {
            self.captured = None;
        }
        if !input.focused {
            self.suspend();
            return Ok(vec![]);
        }
        let next = input.pressed("ui_next") || input.pressed("ui_down");
        let previous = input.pressed("ui_previous") || input.pressed("ui_up");
        if (next || previous) && !targets.is_empty() {
            let current = targets
                .iter()
                .position(|t| Some(&t.key) == self.focus.as_ref());
            let index = match (current, previous) {
                (Some(i), false) => (i + 1) % targets.len(),
                (Some(i), true) => (i + targets.len() - 1) % targets.len(),
                (None, false) => 0,
                (None, true) => targets.len() - 1,
            };
            self.focus = Some(targets[index].key.clone());
        }
        let mut events = vec![];
        if input.pressed("ui_click") {
            self.captured = None;
            if let Some(target) = targets.iter().rev().find(|t| t.bounds.hit(input.pointer)) {
                self.focus = Some(target.key.clone());
                if matches!(
                    roots[target.root].1.tree.nodes[target.node].widget,
                    UiWidget::Slider { .. }
                ) {
                    self.captured = Some(target.key.clone());
                }
                if let Some(event) = activate(roots, target, Some(input.pointer), 0) {
                    events.push(event);
                }
            } else {
                self.focus = None;
            }
        } else if input.held("ui_click") {
            if let Some(target) = targets
                .iter()
                .find(|t| Some(&t.key) == self.captured.as_ref())
                && let Some(event) = activate(roots, target, Some(input.pointer), 0)
            {
                events.push(event);
            }
        } else {
            self.captured = None;
        }
        // At most one activation per widget per tick when keyboard and pointer coincide.
        if !input.pressed("ui_click")
            && !input.held("ui_click")
            && let Some(target) = targets.iter().find(|t| Some(&t.key) == self.focus.as_ref())
        {
            let direction =
                i32::from(input.pressed("ui_right")) - i32::from(input.pressed("ui_left"));
            if (direction != 0 || input.pressed("ui_confirm"))
                && let Some(event) = activate(roots, target, None, direction)
            {
                events.push(event);
            }
        }
        Ok(events)
    }
}

fn targets(roots: &[(String, UiRoot)], viewport: [u32; 2]) -> Result<Vec<Target>> {
    if roots.len() > 64 {
        return Err(invalid("UI exceeds 64 roots"));
    }
    let mut order: Vec<_> = (0..roots.len()).collect();
    order.sort_by(|a, b| (roots[*a].1.layer, &roots[*a].0).cmp(&(roots[*b].1.layer, &roots[*b].0)));
    let mut ids = std::collections::BTreeSet::new();
    let mut targets = vec![];
    for index in order {
        let (id, root) = &roots[index];
        if !ids.insert(id) {
            return Err(invalid("Duplicate UI root ID"));
        }
        root.validate()?;
        if !root.visible || !root.enabled {
            continue;
        }
        for bounds in root.tree.layout(viewport)? {
            let node = &root.tree.nodes[bounds.node];
            let hit = intersection(bounds.rect, bounds.clip);
            if bounds.enabled && node.widget.interactive() && hit.width > 0.0 && hit.height > 0.0 {
                targets.push(Target {
                    root: index,
                    node: bounds.node,
                    key: UiInteraction {
                        root: id.clone(),
                        node: node.id.clone(),
                    },
                    bounds,
                });
            }
        }
    }
    Ok(targets)
}

fn activate(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    pointer: Option<[f32; 2]>,
    direction: i32,
) -> Option<UiWidgetEvent> {
    let node = &mut roots[target.root].1.tree.nodes[target.node];
    let action = node.widget.action().to_owned();
    let (name, value) = match &mut node.widget {
        UiWidget::Button { .. } if direction == 0 => ("ui.clicked", Value::Null),
        UiWidget::Toggle { checked, .. } if direction == 0 => {
            *checked = !*checked;
            ("ui.value_changed", Value::Bool(*checked))
        }
        UiWidget::Slider {
            value,
            min,
            max,
            step,
            ..
        } => {
            let previous = *value;
            let candidate = if let Some(pointer) = pointer {
                if !pointer.iter().all(|v| v.is_finite()) {
                    return None;
                }
                let area = layout::inset(target.bounds.rect, node.style.padding);
                if area.width <= 0.0 {
                    return None;
                }
                *min + ((pointer[0] - area.x) / area.width).clamp(0.0, 1.0) * (*max - *min)
            } else if direction != 0 {
                *value + direction as f32 * *step
            } else {
                return None;
            };
            // Calculate in f64 so a tiny positive step cannot overflow quantization.
            let steps = ((candidate as f64 - *min as f64) / *step as f64).round();
            *value = if candidate >= *max {
                *max
            } else if candidate <= *min {
                *min
            } else {
                (steps * *step as f64 + *min as f64).clamp(*min as f64, *max as f64) as f32
            };
            if *value == previous {
                return None;
            }
            ("ui.value_changed", serde_json::json!(*value))
        }
        _ => return None,
    };
    Some(UiWidgetEvent {
        root: target.key.root.clone(),
        node: node.id.clone(),
        name: name.into(),
        action,
        value,
    })
}
