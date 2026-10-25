//! Stylesheet selectors: widget kind, `#id`, `.class` and state pseudo-classes
//! in compound selectors joined by descendant (` `) or child (`>`) combinators,
//! comma-separated into lists. See `docs/ui-styling.md` for the grammar.
use super::*;

/// Longest selector text, in bytes.
pub const MAX_SELECTOR_LEN: usize = 256;
/// Most compound selectors chained by combinators.
pub const MAX_SELECTOR_COMPOUNDS: usize = 4;
/// Most selectors in one comma-separated list.
pub const MAX_SELECTOR_LIST: usize = 8;
/// Cost of matching a simple selector (and applying its style) to one node.
const SELECTOR_BASE_COST: u32 = 8;
/// Added cost of a child combinator.
const CHILD_COST: u32 = 1;

/// Selector names of every widget kind (the serialized `kind` tags).
pub(super) const KIND_NAMES: [&str; 16] = [
    "container",
    "label",
    "rich_label",
    "image",
    "nine_slice",
    "button",
    "toggle",
    "slider",
    "progress",
    "text_input",
    "dropdown",
    "scroll_view",
    "list",
    "virtual_list",
    "tabs",
    "modal_dialog",
];

/// The selector name of a widget kind. Exhaustive on purpose: a new widget
/// must be given a selector name.
pub(super) fn kind_name(widget: &UiWidget) -> &'static str {
    match widget {
        UiWidget::Container => "container",
        UiWidget::Label { .. } => "label",
        UiWidget::RichLabel { .. } => "rich_label",
        UiWidget::Image { .. } => "image",
        UiWidget::NineSlice { .. } => "nine_slice",
        UiWidget::Button { .. } => "button",
        UiWidget::Toggle { .. } => "toggle",
        UiWidget::Slider { .. } => "slider",
        UiWidget::Progress { .. } => "progress",
        UiWidget::TextInput { .. } => "text_input",
        UiWidget::Dropdown { .. } => "dropdown",
        UiWidget::ScrollView { .. } => "scroll_view",
        UiWidget::List { .. } => "list",
        UiWidget::VirtualList { .. } => "virtual_list",
        UiWidget::Tabs { .. } => "tabs",
        UiWidget::ModalDialog { .. } => "modal_dialog",
    }
}

pub(super) fn style_error(message: impl Into<String>) -> Error {
    Error::new("ui.style.invalid", message)
}

fn selector_error(text: &str, why: &str) -> Error {
    Error::new("ui.style.selector", format!("Selector `{text}`: {why}"))
}

/// One compound selector: every present part must match the same node.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Compound {
    kind: Option<&'static str>,
    id: Option<String>,
    classes: Vec<String>,
    states: UiStateFlags,
    /// `:enabled` — the node is not disabled.
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
}

/// A chain of compounds; `combinators[i]` joins `compounds[i]` to
/// `compounds[i + 1]`, and the last compound selects the styled node.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Selector {
    compounds: Vec<Compound>,
    combinators: Vec<Combinator>,
}

/// Per-root data a selector is matched against.
pub(super) struct MatchContext<'a> {
    pub nodes: &'a [UiNode],
    pub parents: &'a [Option<usize>],
    pub states: &'a [UiStateFlags],
}

impl Compound {
    fn matches(&self, cx: &MatchContext<'_>, index: usize) -> bool {
        let node = &cx.nodes[index];
        let state = cx.states[index];
        self.kind.is_none_or(|k| kind_name(&node.widget) == k)
            && self.id.as_ref().is_none_or(|id| *id == node.id)
            && self.classes.iter().all(|c| node.classes.contains(c))
            && state.contains(self.states)
            && (!self.enabled || !state.disabled())
    }
}

impl Selector {
    /// CSS-style specificity packed as `ids << 16 | (classes + pseudo-classes)
    /// << 8 | kinds`; a larger value wins, ties go to the later rule.
    pub fn specificity(&self) -> u32 {
        let (mut ids, mut classes, mut kinds) = (0u32, 0u32, 0u32);
        for c in &self.compounds {
            ids += u32::from(c.id.is_some());
            classes += c.classes.len() as u32 + c.states.0.count_ones() + u32::from(c.enabled);
            kinds += u32::from(c.kind.is_some());
        }
        (ids.min(255) << 16) | (classes.min(255) << 8) | kinds.min(255)
    }

    /// Relative cost of matching this selector against one node: a base cost, one
    /// unit per child combinator and a full tree depth per descendant combinator
    /// (the walk up the ancestors). Themes are budgeted by the sum of these.
    pub fn cost(&self) -> u32 {
        SELECTOR_BASE_COST
            + self
                .combinators
                .iter()
                .map(|c| match c {
                    Combinator::Child => CHILD_COST,
                    Combinator::Descendant => MAX_UI_DEPTH as u32,
                })
                .sum::<u32>()
    }

    /// Whether the selector selects `node`. Combinator steps are resolved over
    /// a frontier of ancestors, so the cost is bounded by chain length times
    /// tree depth (no backtracking blow-up).
    pub fn matches(&self, cx: &MatchContext<'_>, node: usize) -> bool {
        let last = self.compounds.len() - 1;
        if !self.compounds[last].matches(cx, node) {
            return false;
        }
        // Matched nodes of the current compound, nearest to `node` first.
        let mut frontier = vec![node];
        for position in (1..=last).rev() {
            let previous = &self.compounds[position - 1];
            let mut next = Vec::new();
            match self.combinators[position - 1] {
                Combinator::Child => {
                    for f in &frontier {
                        if let Some(p) = cx.parents[*f]
                            && previous.matches(cx, p)
                        {
                            next.push(p);
                        }
                    }
                }
                Combinator::Descendant => {
                    // All frontier nodes lie on one ancestor chain, so the
                    // strict ancestors of the nearest one cover all of them.
                    let mut cursor = frontier.first().and_then(|f| cx.parents[*f]);
                    while let Some(p) = cursor {
                        if previous.matches(cx, p) {
                            next.push(p);
                        }
                        cursor = cx.parents[p];
                    }
                }
            }
            if next.is_empty() {
                return false;
            }
            frontier = next;
        }
        true
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

fn pseudo(name: &str, into: &mut Compound) -> bool {
    let flag = match name {
        "hover" | "hovered" => UiStateFlags::HOVERED,
        "pressed" | "active" => UiStateFlags::PRESSED,
        "focus" | "focused" => UiStateFlags::FOCUSED,
        "disabled" => UiStateFlags::DISABLED,
        "checked" => UiStateFlags::CHECKED,
        "open" => UiStateFlags::OPEN,
        "selected" => UiStateFlags::SELECTED,
        "enabled" => {
            into.enabled = true;
            return true;
        }
        _ => return false,
    };
    into.states.insert(flag);
    true
}

fn parse_compound(text: &str, chars: &[char], mut i: usize) -> Result<(Compound, usize)> {
    let mut compound = Compound::default();
    let mut parts = 0;
    let ident = |i: &mut usize| -> String {
        let start = *i;
        while *i < chars.len() && is_ident_char(chars[*i]) {
            *i += 1;
        }
        chars[start..*i].iter().collect()
    };
    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '>' {
        match chars[i] {
            '*' if parts == 0 => {
                i += 1;
            }
            '#' => {
                i += 1;
                let id = ident(&mut i);
                if id.is_empty() || compound.id.replace(id).is_some() {
                    return Err(selector_error(text, "bad or repeated #id"));
                }
            }
            '.' => {
                i += 1;
                let class = ident(&mut i);
                if class.is_empty() || class.len() > 64 {
                    return Err(selector_error(text, "bad .class"));
                }
                compound.classes.push(class);
            }
            ':' => {
                i += 1;
                let name = ident(&mut i);
                if !pseudo(&name, &mut compound) {
                    return Err(selector_error(
                        text,
                        &format!("unknown pseudo-class :{name}"),
                    ));
                }
            }
            c if is_ident_char(c) && parts == 0 => {
                let name = ident(&mut i);
                let kind = KIND_NAMES
                    .iter()
                    .find(|k| **k == name)
                    .ok_or_else(|| selector_error(text, &format!("unknown widget kind {name}")))?;
                compound.kind = Some(kind);
            }
            c => {
                return Err(selector_error(
                    text,
                    &format!("unexpected character `{c}` (a kind must come first)"),
                ));
            }
        }
        parts += 1;
    }
    if parts == 0 {
        return Err(selector_error(text, "empty compound selector"));
    }
    Ok((compound, i))
}

fn parse_selector(text: &str) -> Result<Selector> {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut compounds = vec![];
    let mut combinators = vec![];
    let mut i = 0;
    loop {
        let (compound, next) = parse_compound(trimmed, &chars, i)?;
        compounds.push(compound);
        i = next;
        let before = i;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut child = false;
        if i < chars.len() && chars[i] == '>' {
            child = true;
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
        if i >= chars.len() {
            if child {
                return Err(selector_error(trimmed, "trailing `>`"));
            }
            break;
        }
        if !child && i == before {
            return Err(selector_error(trimmed, "unexpected character"));
        }
        combinators.push(if child {
            Combinator::Child
        } else {
            Combinator::Descendant
        });
        if compounds.len() >= MAX_SELECTOR_COMPOUNDS {
            return Err(selector_error(
                trimmed,
                "more than 4 chained compound selectors",
            ));
        }
    }
    Ok(Selector {
        compounds,
        combinators,
    })
}

/// Parse a comma-separated selector list.
pub(super) fn parse_list(text: &str) -> Result<Vec<Selector>> {
    if text.len() > MAX_SELECTOR_LEN {
        let head: String = text.chars().take(32).collect();
        return Err(selector_error(&head, "longer than 256 bytes"));
    }
    let list: Vec<&str> = text.split(',').collect();
    if list.len() > MAX_SELECTOR_LIST {
        return Err(selector_error(text, "more than 8 selectors in a list"));
    }
    list.into_iter().map(parse_selector).collect()
}
