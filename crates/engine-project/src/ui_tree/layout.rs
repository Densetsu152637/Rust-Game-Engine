use super::*;
use std::collections::BTreeMap;

/// A visible node's logical-pixel rectangle and ancestor clip, in paint order.
#[derive(Clone, Debug, PartialEq)]
pub struct UiBox {
    pub node: usize,
    pub rect: Rect,
    pub clip: Rect,
    pub enabled: bool,
}

impl UiBox {
    pub fn hit(&self, point: [f32; 2]) -> bool {
        self.enabled && contains(self.rect, point) && contains(self.clip, point)
    }
}

struct Hierarchy {
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

fn valid_length(length: UiLength) -> bool {
    match length {
        UiLength::Fill => true,
        UiLength::Pixels(n) => n.is_finite() && (0.0..=16384.0).contains(&n),
        UiLength::Percent(n) => n.is_finite() && (0.0..=1.0).contains(&n),
    }
}

fn validate_node(node: &UiNode) -> Result<()> {
    let s = &node.style;
    if node.id.is_empty()
        || node.id.len() > 128
        || !valid_length(s.width)
        || !valid_length(s.height)
        || s.offset.iter().any(|v| !v.is_finite() || v.abs() > 16384.0)
        || [s.padding, s.gap]
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=16384.0).contains(v))
        || !s.font_scale.is_finite()
        || !(0.01..=16.0).contains(&s.font_scale)
        || s.font.len() > 128
        || [s.background, s.foreground, s.accent]
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        || node.widget.text().is_some_and(|s| s.len() > 4096)
        || node.widget.action().len() > 128
    {
        return Err(invalid(format!(
            "Invalid metrics, color, text or ID on UI node {}",
            node.id
        )));
    }
    match node.widget {
        UiWidget::Slider {
            value,
            min,
            max,
            step,
            ..
        } => {
            if !step.is_finite() || step <= 0.0 || step > max - min {
                return Err(invalid("Slider step must be positive and within its range"));
            }
            validate_range(value, min, max)?;
        }
        UiWidget::Progress { value, min, max } => validate_range(value, min, max)?,
        _ => {}
    }
    Ok(())
}

fn validate_range(value: f32, min: f32, max: f32) -> Result<()> {
    if ![value, min, max, max - min].iter().all(|v| v.is_finite())
        || min >= max
        || value < min
        || value > max
    {
        return Err(invalid(
            "Widget range must be finite, increasing and contain its value",
        ));
    }
    Ok(())
}

impl UiTree {
    fn hierarchy(&self) -> Result<Hierarchy> {
        if self.nodes.len() > MAX_UI_NODES {
            return Err(invalid("UI exceeds 1024 nodes"));
        }
        if self
            .nodes
            .iter()
            .map(|n| n.widget.text().map_or(0, str::len))
            .sum::<usize>()
            > 65536
        {
            return Err(invalid("UI root text exceeds 64KiB"));
        }
        let mut ids = BTreeMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            validate_node(node)?;
            if ids.insert(node.id.as_str(), index).is_some() {
                return Err(invalid("Duplicate UI node ID"));
            }
        }
        let mut hierarchy = Hierarchy {
            children: vec![vec![]; self.nodes.len()],
            roots: vec![],
        };
        let mut parents = vec![None; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if let Some(parent) = &node.parent {
                let parent = *ids
                    .get(parent.as_str())
                    .ok_or_else(|| invalid("Unknown UI parent"))?;
                parents[index] = Some(parent);
                hierarchy.children[parent].push(index);
            } else {
                hierarchy.roots.push(index);
            }
        }
        // Bound every ancestor walk: cycles and excessive depth fail without recursion.
        for index in 0..self.nodes.len() {
            let mut cursor = Some(index);
            let mut depth = 0;
            while let Some(node) = cursor {
                depth += 1;
                if depth > MAX_UI_DEPTH {
                    return Err(invalid("UI cycle or depth exceeds 64"));
                }
                cursor = parents[node];
            }
        }
        Ok(hierarchy)
    }

    pub fn validate(&self) -> Result<()> {
        self.hierarchy().map(|_| ())
    }

    pub fn layout(&self, viewport: [u32; 2]) -> Result<Vec<UiBox>> {
        if viewport.contains(&0) || viewport.iter().any(|v| *v > 16384) {
            return Err(invalid("Invalid UI viewport"));
        }
        let hierarchy = self.hierarchy()?;
        let viewport = Rect {
            x: 0.0,
            y: 0.0,
            width: viewport[0] as f32,
            height: viewport[1] as f32,
        };
        let mut pending = self.place_children(
            &hierarchy.roots,
            viewport,
            UiLayout::Absolute,
            0.0,
            viewport,
            true,
        );
        pending.reverse();
        let mut boxes = Vec::with_capacity(self.nodes.len());
        while let Some(item) = pending.pop() {
            let node = &self.nodes[item.node];
            if !node.visible {
                continue;
            }
            let style = &node.style;
            let inner = inset(item.rect, style.padding);
            let clip = if style.clip {
                intersection(item.clip, item.rect)
            } else {
                item.clip
            };
            let mut children = self.place_children(
                &hierarchy.children[item.node],
                inner,
                style.layout,
                style.gap,
                clip,
                item.enabled,
            );
            children.reverse();
            pending.extend(children);
            boxes.push(item);
        }
        Ok(boxes)
    }

    fn place_children(
        &self,
        children: &[usize],
        area: Rect,
        layout: UiLayout,
        gap: f32,
        clip: Rect,
        enabled: bool,
    ) -> Vec<UiBox> {
        let children: Vec<_> = children
            .iter()
            .copied()
            .filter(|i| self.nodes[*i].visible)
            .collect();
        let horizontal = layout == UiLayout::Row;
        let main = if horizontal { area.width } else { area.height };
        let main_length = |s: &UiStyle| if horizontal { s.width } else { s.height };
        let fills = children
            .iter()
            .filter(|i| main_length(&self.nodes[**i].style) == UiLength::Fill)
            .count();
        let fixed: f32 = children
            .iter()
            .map(|i| resolve(main_length(&self.nodes[*i].style), main, 0.0))
            .sum();
        let remaining = (main - fixed - gap * children.len().saturating_sub(1) as f32).max(0.0);
        let share = if fills > 0 {
            remaining / fills as f32
        } else {
            0.0
        };
        let mut cursor = 0.0;
        children
            .into_iter()
            .map(|index| {
                let node = &self.nodes[index];
                let s = &node.style;
                let width = resolve(
                    s.width,
                    area.width,
                    if layout == UiLayout::Row {
                        share
                    } else {
                        area.width
                    },
                );
                let height = resolve(
                    s.height,
                    area.height,
                    if layout == UiLayout::Column {
                        share
                    } else {
                        area.height
                    },
                );
                let rect = Rect {
                    x: area.x + s.offset[0] + if layout == UiLayout::Row { cursor } else { 0.0 },
                    y: area.y
                        + s.offset[1]
                        + if layout == UiLayout::Column {
                            cursor
                        } else {
                            0.0
                        },
                    width,
                    height,
                };
                if layout != UiLayout::Absolute {
                    cursor += if horizontal { width } else { height };
                    cursor += gap;
                }
                UiBox {
                    node: index,
                    rect,
                    clip,
                    enabled: enabled && node.enabled,
                }
            })
            .collect()
    }
}

pub(super) fn inset(rect: Rect, padding: f32) -> Rect {
    let x = padding.min(rect.width / 2.0);
    let y = padding.min(rect.height / 2.0);
    Rect {
        x: rect.x + x,
        y: rect.y + y,
        width: (rect.width - x * 2.0).max(0.0),
        height: (rect.height - y * 2.0).max(0.0),
    }
}

fn resolve(length: UiLength, available: f32, fill: f32) -> f32 {
    match length {
        UiLength::Pixels(n) => n,
        UiLength::Percent(n) => n * available,
        UiLength::Fill => fill,
    }
}
