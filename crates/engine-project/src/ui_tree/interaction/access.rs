//! Assistive-technology requests and the accessibility tree of `GameUi`.
//! Requests apply through the same functions as keyboard and pointer input
//! (`activate`, `select`, `close_dialog`, `scroll_to`), so the same widget
//! events fire.
use super::*;

/// Longest root or node ID accepted in a request.
const MAX_ID: usize = 256;

impl GameUi {
    /// Build the accessibility tree on every tick (enabled by the platform
    /// bridge when an assistive technology connects, or by the settings).
    pub fn set_accessibility(&mut self, enabled: bool) {
        self.access.enabled = enabled;
        if !enabled && !self.access.hint {
            self.access.tree = None;
        }
    }

    pub fn accessibility_enabled(&self) -> bool {
        self.access.enabled || self.access.hint
    }

    pub(crate) fn set_accessibility_hint(&mut self, hint: bool) {
        self.access.hint = hint;
        if !hint && !self.access.enabled {
            self.access.tree = None;
        }
    }

    /// The tree of the last tick; `None` until accessibility is enabled and a
    /// tick has run.
    pub fn accessibility_tree(&self) -> Option<&AccessTree> {
        self.access.tree.as_ref()
    }

    /// Queue an assistive-technology request for the next tick.
    pub fn request_access(&mut self, request: UiAccessRequest) -> Result<()> {
        let bad = |why: &str| Err(Error::new("ui.access.invalid", why.to_owned()));
        if request.target.root.len() > MAX_ID || request.target.node.len() > MAX_ID {
            return bad("Accessibility target ID too long");
        }
        match &request.action {
            UiAccessAction::SetText(text) if text.len() > MAX_UI_TEXT => {
                return bad("Accessibility text exceeds 4096 bytes");
            }
            UiAccessAction::SetNumeric(v) if !v.is_finite() => {
                return bad("Accessibility value must be finite");
            }
            _ => {}
        }
        if self.access.requests.len() >= MAX_ACCESS_REQUESTS {
            return Err(Error::new(
                "ui.access.overflow",
                "More than 64 accessibility requests queued in one tick",
            ));
        }
        self.access.requests.push_back(request);
        Ok(())
    }

    /// What became of the requests applied since the last call (at most 64).
    pub fn take_access_outcomes(&mut self) -> Vec<UiAccessOutcome> {
        std::mem::take(&mut self.access.outcomes)
    }

    pub(super) fn apply_access(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        while let Some(request) = self.access.requests.pop_front() {
            let status = self.apply_request(roots, frame, &request, events);
            if self.access.outcomes.len() >= MAX_ACCESS_OUTCOMES {
                self.access.outcomes.remove(0);
            }
            self.access
                .outcomes
                .push(UiAccessOutcome { request, status });
        }
    }

    pub(super) fn build_access(&mut self, roots: &[(String, UiRoot)], frame: &Frame) {
        if !self.accessibility_enabled() {
            return;
        }
        let tree = {
            let popup = self.popup.as_ref().and_then(|p| {
                let t = frame.find(&p.key)?;
                popup_geometry(roots, &frame.targets[t], p, frame.viewport)
                    .map(|geometry| (&p.key, geometry))
            });
            AccessTree::build(&AccessInput {
                roots,
                frames: frame.roots.iter().map(|rf| (rf.root, &rf.laid)).collect(),
                focus: self.focus.as_ref(),
                popup,
                modal: frame
                    .modal
                    .map(|m| (frame.targets[m].frame, frame.targets[m].node)),
                viewport: frame.viewport,
                scale: self.scale(),
            })
        };
        self.access.tree = Some(tree);
    }

    fn apply_request(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        request: &UiAccessRequest,
        events: &mut Vec<UiWidgetEvent>,
    ) -> UiAccessStatus {
        let id = &request.target;
        if request.action == UiAccessAction::ScrollIntoView {
            return if self.scroll_into_view(roots, frame, &id.root, &id.node, id.item, events) {
                UiAccessStatus::Applied
            } else {
                UiAccessStatus::NotFound
            };
        }
        let key = UiInteraction {
            root: id.root.clone(),
            node: id.node.clone(),
        };
        let target_index = frame.find(&key).filter(|_| id.item.is_none());
        let Some(t) = target_index else {
            return self.apply_indirect(roots, frame, request, events);
        };
        if !frame.allowed(t) {
            return UiAccessStatus::Blocked;
        }
        let target = &frame.targets[t];
        let widget = node(roots, target).widget.clone();
        if target.focusable
            && !matches!(
                request.action,
                UiAccessAction::Expand | UiAccessAction::Collapse
            )
        {
            // Pointer presses focus a focusable widget; so do these.
            self.focus = Some(target.key.clone());
        }
        match (&request.action, id.part) {
            (UiAccessAction::Focus, _) => {
                if target.focusable {
                    UiAccessStatus::Applied
                } else {
                    UiAccessStatus::Unsupported
                }
            }
            (UiAccessAction::Click, None) => match widget {
                UiWidget::Button { .. } | UiWidget::Toggle { .. } => {
                    events.extend(activate(roots, target, None, 0));
                    UiAccessStatus::Applied
                }
                UiWidget::Dropdown {
                    selected, options, ..
                } => {
                    self.show_popup(target, selected, options.len());
                    UiAccessStatus::Applied
                }
                UiWidget::TextInput { .. } => UiAccessStatus::Applied,
                UiWidget::RichLabel { .. } => {
                    let ctx = UiTickContext {
                        viewport: frame.viewport,
                        dt: 0.0,
                        text: &NullTextBackend,
                    };
                    match link_regions(roots, target, &ctx).into_iter().next() {
                        Some(link) => {
                            events.push(link_event(roots, target, &link.id));
                            UiAccessStatus::Applied
                        }
                        None => UiAccessStatus::Unsupported,
                    }
                }
                _ => UiAccessStatus::Unsupported,
            },
            (UiAccessAction::Click, Some(AccessPart::Tab(i))) => match widget {
                UiWidget::Tabs { labels, .. } if (i as usize) < labels.len() => {
                    events.extend(select(roots, target, i, None));
                    UiAccessStatus::Applied
                }
                _ => UiAccessStatus::NotFound,
            },
            (UiAccessAction::Click, Some(AccessPart::Button(i))) => match widget {
                UiWidget::ModalDialog { buttons, .. } if (i as usize) < buttons.len() => {
                    events.extend(close_dialog(roots, target, i64::from(i)));
                    UiAccessStatus::Applied
                }
                _ => UiAccessStatus::NotFound,
            },
            (UiAccessAction::Click, Some(AccessPart::Option(i))) => match widget {
                UiWidget::Dropdown { options, .. } if (i as usize) < options.len() => {
                    self.choose_option(roots, target, i, events);
                    UiAccessStatus::Applied
                }
                _ => UiAccessStatus::NotFound,
            },
            (UiAccessAction::Increment | UiAccessAction::Decrement, None) => {
                let direction = if request.action == UiAccessAction::Increment {
                    1
                } else {
                    -1
                };
                match widget {
                    UiWidget::Slider { .. } => {
                        events.extend(activate(roots, target, None, direction));
                        UiAccessStatus::Applied
                    }
                    _ => UiAccessStatus::Unsupported,
                }
            }
            (UiAccessAction::SetNumeric(value), None) => match widget {
                UiWidget::Slider { .. } => set_numeric(roots, target, *value, events),
                _ => UiAccessStatus::Unsupported,
            },
            (UiAccessAction::SetText(text), None) => match widget {
                UiWidget::TextInput { .. } => {
                    if self.preedit.as_ref().is_some_and(|(k, _)| *k == target.key) {
                        self.preedit = None;
                    }
                    set_text(roots, target, text, events)
                }
                _ => UiAccessStatus::Unsupported,
            },
            (UiAccessAction::Expand, None) => match widget {
                UiWidget::Dropdown {
                    selected, options, ..
                } => {
                    self.show_popup(target, selected, options.len());
                    UiAccessStatus::Applied
                }
                _ => UiAccessStatus::Unsupported,
            },
            (UiAccessAction::Collapse, None) => match widget {
                UiWidget::Dropdown { .. } => {
                    if self.popup.as_ref().is_some_and(|p| p.key == target.key) {
                        self.popup = None;
                    }
                    UiAccessStatus::Applied
                }
                _ => UiAccessStatus::Unsupported,
            },
            _ => UiAccessStatus::Unsupported,
        }
    }

    /// Requests for nodes that are not input targets: list items (click
    /// selects), and nodes that are disabled or missing.
    fn apply_indirect(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        request: &UiAccessRequest,
        events: &mut Vec<UiWidgetEvent>,
    ) -> UiAccessStatus {
        let id = &request.target;
        let Some((f, n)) = locate(roots, frame, &id.root, &id.node) else {
            return UiAccessStatus::NotFound;
        };
        let laid = &frame.roots[f].laid;
        let Some(bounds) = laid.boxes.iter().find(|b| b.node == n && b.item == id.item) else {
            return UiAccessStatus::NotFound;
        };
        let disabled = !bounds.enabled;
        let nodes = &roots[frame.roots[f].root].1.tree.nodes;
        let index = match laid.hierarchy.parents[n] {
            Some(p) => match &nodes[p].widget {
                UiWidget::List { .. } => laid.hierarchy.children[p].iter().position(|c| *c == n),
                UiWidget::VirtualList { .. } if laid.hierarchy.children[p].first() == Some(&n) => {
                    id.item.map(|i| i as usize)
                }
                _ => None,
            }
            .map(|i| (p, i)),
            None => None,
        };
        let supported = matches!(
            request.action,
            UiAccessAction::Click | UiAccessAction::Focus
        );
        match index {
            Some((p, i)) if supported && !disabled => {
                let owner = UiInteraction {
                    root: id.root.clone(),
                    node: nodes[p].id.clone(),
                };
                let Some(t) = frame.find(&owner) else {
                    return UiAccessStatus::Blocked;
                };
                if !frame.allowed(t) {
                    return UiAccessStatus::Blocked;
                }
                let target = &frame.targets[t];
                if target.focusable {
                    self.focus = Some(owner);
                }
                if request.action == UiAccessAction::Click {
                    events.extend(select(roots, target, i as u32, None));
                }
                UiAccessStatus::Applied
            }
            _ if disabled => UiAccessStatus::Blocked,
            _ => UiAccessStatus::Unsupported,
        }
    }

    fn show_popup(&mut self, target: &Target, selected: Option<u32>, options: usize) {
        if options > 0 {
            let highlight = selected.unwrap_or(0);
            self.popup = Some(PopupState {
                key: target.key.clone(),
                highlight,
                first: highlight,
            });
        }
    }

    fn choose_option(
        &mut self,
        roots: &mut [(String, UiRoot)],
        target: &Target,
        index: u32,
        events: &mut Vec<UiWidgetEvent>,
    ) {
        let node = node_mut(roots, target);
        if let UiWidget::Dropdown { selected, .. } = &mut node.widget
            && *selected != Some(index)
        {
            *selected = Some(index);
            let node = &*node;
            events.push(event(target, node, "ui.selection_changed", json!(index)));
        }
        if self.popup.as_ref().is_some_and(|p| p.key == target.key) {
            self.popup = None;
        }
    }

    /// Scroll the scroll views and virtual lists above a node so that its
    /// box is inside their visible area. `false` when the node has no box.
    pub(super) fn scroll_into_view(
        &mut self,
        roots: &mut [(String, UiRoot)],
        frame: &Frame,
        root: &str,
        node_id: &str,
        item: Option<u32>,
        events: &mut Vec<UiWidgetEvent>,
    ) -> bool {
        let Some((f, n)) = locate(roots, frame, root, node_id) else {
            return false;
        };
        let laid = &frame.roots[f].laid;
        let Some(mut rect) = laid
            .boxes
            .iter()
            .find(|b| b.node == n && b.item == item)
            .or_else(|| laid.boxes.iter().find(|b| b.node == n))
            .map(|b| b.rect)
        else {
            return false;
        };
        let ancestors: Vec<usize> = laid.hierarchy.ancestors(n).collect();
        for ancestor in ancestors {
            let (horizontal, vertical) =
                match &roots[frame.roots[f].root].1.tree.nodes[ancestor].widget {
                    UiWidget::ScrollView { axes, .. } => (axes.horizontal(), axes.vertical()),
                    UiWidget::VirtualList { horizontal, .. } => (*horizontal, !*horizontal),
                    _ => continue,
                };
            let Some(bounds) = laid
                .boxes
                .iter()
                .find(|b| b.node == ancestor && b.item == item)
                .or_else(|| {
                    laid.boxes
                        .iter()
                        .find(|b| b.node == ancestor && b.item.is_none())
                })
            else {
                continue;
            };
            let Some(target) = frame
                .targets
                .iter()
                .find(|t| t.frame == f && t.node == ancestor)
            else {
                continue;
            };
            let view = bounds.content;
            let shift = |pos: f32, len: f32, view_pos: f32, view_len: f32| {
                if len > view_len || pos < view_pos {
                    pos - view_pos
                } else if pos + len > view_pos + view_len {
                    pos + len - (view_pos + view_len)
                } else {
                    0.0
                }
            };
            let dx = if horizontal {
                shift(rect.x, rect.width, view.x, view.width)
            } else {
                0.0
            };
            let dy = if vertical {
                shift(rect.y, rect.height, view.y, view.height)
            } else {
                0.0
            };
            if dx == 0.0 && dy == 0.0 {
                continue;
            }
            let max = target.bounds.scroll_max;
            let from = bounds.scroll;
            let to = [
                (from[0] + dx).clamp(0.0, max[0]),
                (from[1] + dy).clamp(0.0, max[1]),
            ];
            events.extend(scroll_to(roots, target, to));
            rect.x -= to[0] - from[0];
            rect.y -= to[1] - from[1];
        }
        true
    }
}

/// Frame index and node index of `node_id` in root `root`.
fn locate(
    roots: &[(String, UiRoot)],
    frame: &Frame,
    root: &str,
    node_id: &str,
) -> Option<(usize, usize)> {
    let f = frame.roots.iter().position(|rf| roots[rf.root].0 == root)?;
    let n = *frame.roots[f].laid.hierarchy.index.get(node_id)?;
    Some((f, n))
}

fn set_numeric(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    requested: f64,
    events: &mut Vec<UiWidgetEvent>,
) -> UiAccessStatus {
    let node = node_mut(roots, target);
    let UiWidget::Slider {
        value,
        min,
        max,
        step,
        ..
    } = &mut node.widget
    else {
        return UiAccessStatus::Unsupported;
    };
    if !requested.is_finite() || requested < f64::from(*min) || requested > f64::from(*max) {
        return UiAccessStatus::Rejected;
    }
    let previous = *value;
    *value = quantize(requested as f32, *min, *max, *step);
    if *value != previous {
        let current = *value;
        let node = &*node;
        events.push(event(target, node, "ui.value_changed", json!(current)));
    }
    UiAccessStatus::Applied
}

fn set_text(
    roots: &mut [(String, UiRoot)],
    target: &Target,
    text: &str,
    events: &mut Vec<UiWidgetEvent>,
) -> UiAccessStatus {
    let spare = MAX_UI_ROOT_TEXT.saturating_sub(roots[target.root].1.tree.text_bytes());
    let node = node_mut(roots, target);
    let UiWidget::TextInput {
        text: value,
        caret,
        anchor,
        max_length,
        multiline,
        ..
    } = &mut node.widget
    else {
        return UiAccessStatus::Unsupported;
    };
    let chars = text.chars().count() as u32;
    // `maxLength` counts characters; the value is also bounded in bytes by the
    // string limit and by what is left of the root's text budget.
    if chars > *max_length
        || text.len() > MAX_UI_TEXT
        || text.len() > value.len() + spare
        || (!*multiline && text.contains('\n'))
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return UiAccessStatus::Rejected;
    }
    let changed = value != text;
    *value = text.to_owned();
    *caret = chars;
    *anchor = chars;
    if changed {
        let node = &*node;
        events.push(event(target, node, "ui.text_changed", json!(text)));
    }
    UiAccessStatus::Applied
}
