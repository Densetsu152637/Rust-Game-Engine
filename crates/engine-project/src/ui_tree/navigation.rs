//! Keyboard/gamepad focus movement: tab order and spatial (directional) search.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiDirection {
    Up,
    Down,
    Left,
    Right,
}

impl UiDirection {
    /// Explicit override on a node for this direction.
    pub(super) fn target(self, nav: &UiNavigation) -> Option<&str> {
        match self {
            Self::Up => nav.up.as_deref(),
            Self::Down => nav.down.as_deref(),
            Self::Left => nav.left.as_deref(),
            Self::Right => nav.right.as_deref(),
        }
    }
    /// Unit vector in screen space (Y down).
    fn axis(self) -> [f32; 2] {
        match self {
            Self::Up => [0.0, -1.0],
            Self::Down => [0.0, 1.0],
            Self::Left => [-1.0, 0.0],
            Self::Right => [1.0, 0.0],
        }
    }
}

/// Next index in tab order. `order` holds `(tab_index, paint_index)` per
/// candidate; ordering is ascending by both. Wraps around.
pub(super) fn tab_step(
    order: &[(i32, usize)],
    current: Option<usize>,
    back: bool,
) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    let mut sorted: Vec<usize> = (0..order.len()).collect();
    sorted.sort_by_key(|i| order[*i]);
    let position = current.and_then(|c| sorted.iter().position(|i| *i == c));
    let next = match (position, back) {
        (Some(p), false) => (p + 1) % sorted.len(),
        (Some(p), true) => (p + sorted.len() - 1) % sorted.len(),
        (None, false) => 0,
        (None, true) => sorted.len() - 1,
    };
    Some(sorted[next])
}

fn center(r: Rect) -> [f32; 2] {
    [r.x + r.width / 2.0, r.y + r.height / 2.0]
}

/// Gap between two 1-D ranges (0 when they overlap).
fn gap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (b0 - a1).max(a0 - b1).max(0.0)
}

/// Nearest candidate in `direction` from `from`. A candidate qualifies when its
/// centre lies strictly beyond `from`'s centre along the direction. The score
/// is the centre distance along the direction plus twice the edge gap on the
/// perpendicular axis, so aligned neighbours win over closer diagonal ones.
/// Ties keep the earliest candidate (paint order).
pub(super) fn spatial(
    from: Rect,
    candidates: impl IntoIterator<Item = (usize, Rect)>,
    direction: UiDirection,
) -> Option<usize> {
    let [dx, dy] = direction.axis();
    let origin = center(from);
    let mut best: Option<(f32, usize)> = None;
    for (index, rect) in candidates {
        let c = center(rect);
        let primary = (c[0] - origin[0]) * dx + (c[1] - origin[1]) * dy;
        if primary <= 0.5 {
            continue;
        }
        let orthogonal = if dx != 0.0 {
            gap(from.y, from.y + from.height, rect.y, rect.y + rect.height)
        } else {
            gap(from.x, from.x + from.width, rect.x, rect.x + rect.width)
        };
        let score = primary + 2.0 * orthogonal;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, index));
        }
    }
    best.map(|(_, index)| index)
}
