//! Layout inspection for authoring tools: computed boxes per node, hit
//! testing and layout issues (`docs/ui-authoring.md`).
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Issues reported per inspection.
pub const MAX_UI_ISSUES: usize = 200;
/// Overlap tolerated before a child counts as overflowing, in pixels.
const OVERFLOW_TOLERANCE: f32 = 0.5;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiIssueSeverity {
    Warning,
    Info,
}

/// A layout finding. `code` is one of `ui.issue.overflow`, `ui.issue.zero_size`,
/// `ui.issue.clipped_away` (no visible area: clipped by an ancestor or
/// outside the viewport), `ui.issue.unreachable_focus` and
/// `ui.issue.nav_target_not_focusable`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UiIssue {
    pub code: String,
    pub severity: UiIssueSeverity,
    pub node: String,
    pub message: String,
}

/// The computed layout of one visible node.
#[derive(Clone, Debug, PartialEq)]
pub struct UiNodeLayout {
    pub id: String,
    pub parent: Option<String>,
    /// Widget `kind`.
    pub kind: &'static str,
    /// Ancestors above this node.
    pub depth: usize,
    /// Border box in logical pixels.
    pub rect: Rect,
    /// Border box minus padding and widget chrome.
    pub content: Rect,
    /// Intersection of the ancestors' clips.
    pub clip: Rect,
    /// `rect` clipped by `clip`: the part that can be seen and hit.
    pub visible_rect: Rect,
    /// The node and all its ancestors are enabled.
    pub enabled: bool,
    /// Part of an open modal dialog, painted above the base tree.
    pub overlay: bool,
    /// Virtual-list item index of a template replica.
    pub item: Option<u32>,
    pub scroll: [f32; 2],
    pub scroll_max: [f32; 2],
    /// Index in paint order (later paints above).
    pub paint: usize,
    pub states: UiStateFlags,
}

/// Which nodes [`UiInspection::hit_test`] considers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiHitMode {
    /// Any visible box, including containers and disabled nodes.
    Any,
    /// Only enabled nodes that receive pointer presses.
    Interactive,
}

/// Layout of a whole root at one viewport.
#[derive(Clone, Debug)]
pub struct UiInspection {
    pub viewport: [u32; 2],
    /// Boxes in paint order.
    pub nodes: Vec<UiNodeLayout>,
    /// IDs of nodes with no box: hidden, in an unselected tab, or a closed dialog.
    pub unplaced: Vec<String>,
    pub issues: Vec<UiIssue>,
    /// More issues existed than [`MAX_UI_ISSUES`].
    pub issues_truncated: bool,
}

fn rect_json(rect: Rect) -> Value {
    json!([rect.x, rect.y, rect.width, rect.height])
}

impl UiNodeLayout {
    /// `{id, parent, kind, rect:[x,y,w,h], ...}` with finite numbers only.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "id": self.id, "kind": self.kind, "depth": self.depth, "paint": self.paint,
            "rect": rect_json(self.rect), "content": rect_json(self.content),
            "clip": rect_json(self.clip), "visible_rect": rect_json(self.visible_rect),
            "enabled": self.enabled,
        });
        let object = value.as_object_mut().expect("object literal");
        if let Some(parent) = &self.parent {
            object.insert("parent".into(), json!(parent));
        }
        if self.overlay {
            object.insert("overlay".into(), json!(true));
        }
        if let Some(item) = self.item {
            object.insert("item".into(), json!(item));
        }
        if self.scroll_max != [0.0; 2] {
            object.insert("scroll".into(), json!(self.scroll));
            object.insert("scroll_max".into(), json!(self.scroll_max));
        }
        if !self.states.is_empty() {
            object.insert("states".into(), json!(state_names(self.states)));
        }
        value
    }
}

/// Names of the set flags, in `hovered, pressed, focused, disabled, checked, open, selected` order.
pub fn state_names(flags: UiStateFlags) -> Vec<&'static str> {
    [
        (UiStateFlags::HOVERED, "hovered"),
        (UiStateFlags::PRESSED, "pressed"),
        (UiStateFlags::FOCUSED, "focused"),
        (UiStateFlags::DISABLED, "disabled"),
        (UiStateFlags::CHECKED, "checked"),
        (UiStateFlags::OPEN, "open"),
        (UiStateFlags::SELECTED, "selected"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, name)| name)
    .collect()
}

/// The flag named `name` (see [`state_names`]).
pub fn state_flag(name: &str) -> Option<UiStateFlags> {
    Some(match name {
        "hovered" => UiStateFlags::HOVERED,
        "pressed" => UiStateFlags::PRESSED,
        "focused" => UiStateFlags::FOCUSED,
        "disabled" => UiStateFlags::DISABLED,
        "checked" => UiStateFlags::CHECKED,
        "open" => UiStateFlags::OPEN,
        "selected" => UiStateFlags::SELECTED,
        _ => return None,
    })
}

impl UiInspection {
    /// The layout of node `id`, when it has a box.
    pub fn node(&self, id: &str) -> Option<&UiNodeLayout> {
        self.nodes.iter().find(|n| n.id == id && n.item.is_none())
    }

    /// Every node under `point`, topmost first.
    pub fn hit_stack(&self, point: [f32; 2], mode: UiHitMode, root: &UiRoot) -> Vec<&UiNodeLayout> {
        self.nodes
            .iter()
            .rev()
            .filter(|n| contains(n.rect, point) && contains(n.clip, point))
            .filter(|n| {
                mode == UiHitMode::Any
                    || (n.enabled
                        && root
                            .tree
                            .find(&n.id)
                            .is_some_and(|i| root.tree.nodes[i].widget.interactive()))
            })
            .collect()
    }

    /// The topmost node under `point`.
    pub fn hit_test(
        &self,
        point: [f32; 2],
        mode: UiHitMode,
        root: &UiRoot,
    ) -> Option<&UiNodeLayout> {
        self.hit_stack(point, mode, root).into_iter().next()
    }
}

fn positive(rect: Rect) -> bool {
    rect.width > 0.0 && rect.height > 0.0
}

fn scrolls(kind: &UiWidget) -> bool {
    matches!(
        kind,
        UiWidget::ScrollView { .. } | UiWidget::VirtualList { .. }
    )
}

/// Lay `root` out at `viewport` through `text` and describe the result.
/// `states` are the interaction flags to report per node ID.
pub fn inspect_layout(
    root: &UiRoot,
    viewport: [u32; 2],
    text: &dyn UiTextBackend,
    states: &BTreeMap<String, UiStateFlags>,
) -> Result<UiInspection> {
    root.validate()?;
    let laid = root.tree.laid(viewport, text)?;
    let nodes = &root.tree.nodes;
    let hierarchy = &laid.hierarchy;
    let layouts: Vec<UiNodeLayout> = laid
        .boxes
        .iter()
        .enumerate()
        .map(|(paint, b)| UiNodeLayout {
            id: nodes[b.node].id.clone(),
            parent: nodes[b.node].parent.clone(),
            kind: nodes[b.node].widget.kind_name(),
            depth: hierarchy.ancestors(b.node).count(),
            rect: b.rect,
            content: b.content,
            clip: b.clip,
            visible_rect: b.visible(),
            enabled: b.enabled,
            overlay: b.overlay,
            item: b.item,
            scroll: b.scroll,
            scroll_max: b.scroll_max,
            paint,
            states: states.get(&nodes[b.node].id).copied().unwrap_or_default(),
        })
        .collect();
    let placed: std::collections::BTreeSet<usize> = laid.boxes.iter().map(|b| b.node).collect();
    let unplaced = nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| !placed.contains(i))
        .map(|(_, n)| n.id.clone())
        .collect();

    let mut issues = vec![];
    let mut push = |code: &str, severity: UiIssueSeverity, node: &str, message: String| {
        issues.push(UiIssue {
            code: format!("ui.issue.{code}"),
            severity,
            node: node.to_owned(),
            message,
        });
    };
    let modal = laid.boxes.iter().any(|b| b.overlay);
    // The first box of each node (replicas repeat the template).
    let mut seen = std::collections::BTreeSet::new();
    for b in laid.boxes.iter().filter(|b| seen.insert(b.node)) {
        let node = &nodes[b.node];
        let in_scroll = hierarchy
            .ancestors(b.node)
            .any(|a| scrolls(&nodes[a].widget));
        if !positive(b.rect) {
            push(
                "zero_size",
                UiIssueSeverity::Warning,
                &node.id,
                format!("`{}` has a {}x{} box", node.id, b.rect.width, b.rect.height),
            );
        } else if !in_scroll {
            if !positive(b.visible()) && b.item.is_none() {
                push(
                    "clipped_away",
                    UiIssueSeverity::Warning,
                    &node.id,
                    format!(
                        "`{}` is completely clipped by an ancestor or the viewport",
                        node.id
                    ),
                );
            }
            if let Some(parent) = hierarchy.parents[b.node]
                && let Some(parent_box) = laid
                    .boxes
                    .iter()
                    .find(|p| p.node == parent && p.overlay == b.overlay)
            {
                let (r, p) = (b.rect, parent_box.rect);
                let edges = [
                    ("left", p.x - r.x),
                    ("top", p.y - r.y),
                    ("right", (r.x + r.width) - (p.x + p.width)),
                    ("bottom", (r.y + r.height) - (p.y + p.height)),
                ];
                if let Some((side, amount)) = edges
                    .into_iter()
                    .filter(|(_, a)| *a > OVERFLOW_TOLERANCE)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    && !scrolls(&nodes[parent].widget)
                {
                    let clipped = nodes[parent].style.clip;
                    push(
                        "overflow",
                        if clipped {
                            UiIssueSeverity::Warning
                        } else {
                            UiIssueSeverity::Info
                        },
                        &node.id,
                        format!(
                            "`{}` extends {amount:.1}px past the {side} edge of `{}`{}",
                            node.id,
                            nodes[parent].id,
                            if clipped { " and is clipped" } else { "" }
                        ),
                    );
                }
            }
        }
        if node.widget.focusable()
            && node.nav.focusable != Some(false)
            && node.enabled
            && b.item.is_none()
        {
            let reason = if !b.enabled {
                Some("an ancestor or the root is disabled".to_owned())
            } else if modal && !b.overlay {
                Some("an open modal dialog traps focus".to_owned())
            } else if !positive(b.visible()) && !in_scroll {
                Some("it is completely clipped".to_owned())
            } else {
                None
            };
            if let Some(reason) = reason {
                push(
                    "unreachable_focus",
                    UiIssueSeverity::Warning,
                    &node.id,
                    format!("`{}` cannot take keyboard focus: {reason}", node.id),
                );
            }
        }
        for (direction, target) in [
            ("up", &node.nav.up),
            ("down", &node.nav.down),
            ("left", &node.nav.left),
            ("right", &node.nav.right),
        ] {
            let Some(target) = target else { continue };
            let focusable = nodes
                .iter()
                .any(|n| n.id == *target && n.widget.focusable() && n.nav.focusable != Some(false));
            if !focusable {
                push(
                    "nav_target_not_focusable",
                    UiIssueSeverity::Warning,
                    &node.id,
                    format!(
                        "The `{direction}` target `{target}` of `{}` is not focusable",
                        node.id
                    ),
                );
            }
        }
    }
    let truncated = issues.len() > MAX_UI_ISSUES;
    issues.truncate(MAX_UI_ISSUES);
    Ok(UiInspection {
        viewport,
        nodes: layouts,
        unplaced,
        issues,
        issues_truncated: truncated,
    })
}

impl UiDocument {
    /// [`inspect_layout`] of the current root (use [`effective_root`] first to
    /// inspect a themed or localized look).
    pub fn inspect(
        &self,
        viewport: [u32; 2],
        text: &dyn UiTextBackend,
        states: &BTreeMap<String, UiStateFlags>,
    ) -> Result<UiInspection> {
        inspect_layout(self.root(), viewport, text, states)
    }
}
