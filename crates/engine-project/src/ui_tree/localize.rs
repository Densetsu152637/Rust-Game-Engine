//! Localized text and assets in UI trees (`docs/localization.md`).
//!
//! Authored strings stay as authored in `UiRoot`: a text that is exactly
//! `@message.key` is a *message reference* and `@@...` an escaped literal (see
//! [`crate::localization::classify`]). References and per-locale asset overrides
//! are applied to the *effective* root that layout, interaction and drawing read,
//! never to the stored component, so switching language needs no reload and
//! snapshots keep the keys. Nodes whose text a data binding drives are left to
//! the binding (`message` keys go through the string resolver).
//!
//! A translation cannot make a root invalid: every expansion is cut to the
//! limit of its slot (4096 bytes for text, 256 for short labels, 1024 for
//! tooltips) and to what is left of the root's 64 KiB text budget.
use super::*;
use crate::localization::Localization;
use std::collections::BTreeSet;

const MAX_TOOLTIP_TEXT: usize = 1024;

/// Resolution state for one root.
struct Resolver<'a> {
    loc: &'a Localization,
    /// Bytes the root may still grow by before its text budget is full.
    spare: usize,
}

impl Resolver<'_> {
    fn resolve(&mut self, text: &mut String, markup: bool, limit: usize) {
        let Some(mut resolved) = self.loc.resolve_text(text, markup) else {
            return;
        };
        if resolved.len() > limit {
            resolved.truncate(resolved.floor_char_boundary(limit));
        }
        let growth = resolved.len().saturating_sub(text.len());
        if growth > self.spare {
            let keep = resolved.len() - (growth - self.spare);
            resolved.truncate(resolved.floor_char_boundary(keep));
        }
        self.spare = self
            .spare
            .saturating_sub(resolved.len().saturating_sub(text.len()));
        *text = resolved;
    }

    fn texts(&mut self, node: &mut UiNode, bound: bool) {
        match &mut node.widget {
            UiWidget::Label { text }
            | UiWidget::Button { text, .. }
            | UiWidget::Toggle { text, .. }
                if !bound =>
            {
                self.resolve(text, false, MAX_UI_TEXT);
            }
            UiWidget::RichLabel { markup } if !bound => {
                self.resolve(markup, true, MAX_UI_TEXT);
            }
            UiWidget::TextInput { placeholder, .. } => {
                self.resolve(placeholder, false, MAX_UI_LABEL);
            }
            UiWidget::Dropdown {
                options,
                placeholder,
                ..
            } => {
                for option in options {
                    self.resolve(option, false, MAX_UI_LABEL);
                }
                self.resolve(placeholder, false, MAX_UI_LABEL);
            }
            UiWidget::Tabs { labels, .. } => {
                for label in labels {
                    self.resolve(label, false, MAX_UI_LABEL);
                }
            }
            UiWidget::ModalDialog { title, buttons, .. } => {
                self.resolve(title, false, MAX_UI_LABEL);
                for button in buttons {
                    self.resolve(button, false, MAX_UI_LABEL);
                }
            }
            _ => {}
        }
        if let Some(tooltip) = &mut node.tooltip {
            self.resolve(&mut tooltip.text, false, MAX_TOOLTIP_TEXT);
        }
    }
}

fn assets(loc: &Localization, node: &mut UiNode) {
    if let UiWidget::Image { texture, .. } | UiWidget::NineSlice { texture, .. } = &mut node.widget
    {
        *texture = loc.asset(texture);
    }
    if !node.style.font.is_empty() {
        node.style.font = loc.asset(&node.style.font);
    }
}

fn text_total(root: &UiRoot) -> usize {
    root.tree
        .nodes
        .iter()
        .map(|n| n.widget.text_bytes() + n.tooltip.as_ref().map_or(0, |t| t.text.len()))
        .sum()
}

/// `root` with message references resolved and locale asset overrides applied,
/// or `None` when nothing in it changes (the common case, which costs no clone).
pub(super) fn localize(root: &UiRoot, loc: &Localization) -> Option<UiRoot> {
    if !loc.active() {
        return None;
    }
    let overrides = loc.has_asset_overrides();
    let bound: BTreeSet<&str> = root
        .bindings
        .iter()
        .filter(|b| matches!(b.property, UiBindProperty::Text | UiBindProperty::ItemText))
        .map(|b| b.node.as_str())
        .collect();
    let mut resolver = Resolver {
        loc,
        spare: MAX_UI_ROOT_TEXT.saturating_sub(text_total(root)),
    };
    let mut out: Option<UiRoot> = None;
    for (index, node) in root.tree.nodes.iter().enumerate() {
        let text = node.widget.strings().iter().any(|s| s.starts_with('@'))
            || node
                .tooltip
                .as_ref()
                .is_some_and(|t| t.text.starts_with('@'));
        let asset = overrides
            && (matches!(
                node.widget,
                UiWidget::Image { .. } | UiWidget::NineSlice { .. }
            ) || !node.style.font.is_empty());
        if !text && !asset {
            continue;
        }
        let mut next = node.clone();
        if text {
            resolver.texts(&mut next, bound.contains(node.id.as_str()));
        }
        if asset {
            assets(loc, &mut next);
        }
        if next != *node {
            out.get_or_insert_with(|| root.clone()).tree.nodes[index] = next;
        }
    }
    out
}

/// Put the authored localizable strings of `authored` back into `effective`
/// after interaction edited it: widget state (checked, selection, typed text,
/// scroll) flows back into the stored root, resolved text and asset keys do not.
pub(super) fn restore_authored(effective: &mut UiWidget, authored: &UiWidget) {
    match (effective, authored) {
        (UiWidget::Label { text: a }, UiWidget::Label { text: b })
        | (UiWidget::Button { text: a, .. }, UiWidget::Button { text: b, .. })
        | (UiWidget::Toggle { text: a, .. }, UiWidget::Toggle { text: b, .. })
        | (UiWidget::RichLabel { markup: a }, UiWidget::RichLabel { markup: b })
        | (UiWidget::Image { texture: a, .. }, UiWidget::Image { texture: b, .. })
        | (UiWidget::NineSlice { texture: a, .. }, UiWidget::NineSlice { texture: b, .. })
        | (
            UiWidget::TextInput { placeholder: a, .. },
            UiWidget::TextInput { placeholder: b, .. },
        ) => a.clone_from(b),
        (
            UiWidget::Dropdown {
                options: ao,
                placeholder: ap,
                ..
            },
            UiWidget::Dropdown {
                options: bo,
                placeholder: bp,
                ..
            },
        ) => {
            ao.clone_from(bo);
            ap.clone_from(bp);
        }
        (UiWidget::Tabs { labels: a, .. }, UiWidget::Tabs { labels: b, .. }) => a.clone_from(b),
        (
            UiWidget::ModalDialog {
                title: at,
                buttons: ab,
                ..
            },
            UiWidget::ModalDialog {
                title: bt,
                buttons: bb,
                ..
            },
        ) => {
            at.clone_from(bt);
            ab.clone_from(bb);
        }
        _ => {}
    }
}
