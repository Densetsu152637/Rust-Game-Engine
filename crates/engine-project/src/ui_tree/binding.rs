//! Data binding: a JSON-like `UiDataContext` resource, authored `UiBinding`s
//! that connect widget properties to paths in it, change detection, a format
//! template language and the `UiStringResolver` seam for message keys.
use super::{tree::Hierarchy, *};
use bevy_ecs::prelude::{Resource, World};
use serde_json::{Map, Number, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Bindings per root.
pub const MAX_UI_BINDINGS: usize = 512;
pub const MAX_BIND_PATH: usize = 128;
pub const MAX_BIND_SEGMENTS: usize = 16;
pub const MAX_BIND_FORMAT: usize = 256;
pub const MAX_BIND_ARGS: usize = 8;
/// Estimated memory of one data context.
pub const MAX_DATA_BYTES: usize = 16 << 20;
pub const MAX_DATA_DEPTH: usize = 16;
/// Longest string value.
pub const MAX_DATA_STRING: usize = 65536;
/// Longest array and most object keys.
pub const MAX_DATA_ITEMS: usize = 1_000_000;

fn bind_error(message: impl Into<String>) -> Error {
    Error::new("ui.bind.invalid", message)
}

fn path_error(path: &str, why: &str) -> Error {
    let head: String = path.chars().take(48).collect();
    Error::new("ui.bind.path", format!("Path `{head}`: {why}"))
}

/// One step of a data path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiPathSegment {
    Key(String),
    Index(usize),
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// Parse `a.b[2].c`: a key, then `.key` or `[index]` steps. Keys use
/// `[A-Za-z0-9_-]`; at most 128 bytes and 16 segments.
pub fn parse_path(text: &str) -> Result<Vec<UiPathSegment>> {
    if text.is_empty() || text.len() > MAX_BIND_PATH {
        return Err(path_error(text, "empty or longer than 128 bytes"));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut segments = vec![];
    let mut i = 0;
    let key = |i: &mut usize| -> Option<String> {
        let start = *i;
        while *i < chars.len() && key_char(chars[*i]) {
            *i += 1;
        }
        (*i > start).then(|| chars[start..*i].iter().collect())
    };
    segments.push(UiPathSegment::Key(
        key(&mut i).ok_or_else(|| path_error(text, "must start with a key"))?,
    ));
    while i < chars.len() {
        match chars[i] {
            '.' => {
                i += 1;
                segments.push(UiPathSegment::Key(
                    key(&mut i).ok_or_else(|| path_error(text, "empty key after `.`"))?,
                ));
            }
            '[' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start || i >= chars.len() || chars[i] != ']' || i - start > 7 {
                    return Err(path_error(text, "bad [index]"));
                }
                let index: usize = chars[start..i]
                    .iter()
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                i += 1;
                segments.push(UiPathSegment::Index(index));
            }
            c => return Err(path_error(text, &format!("unexpected character `{c}`"))),
        }
    }
    if segments.len() > MAX_BIND_SEGMENTS {
        return Err(path_error(text, "more than 16 segments"));
    }
    Ok(segments)
}

fn walk<'a>(mut value: &'a Value, segments: &[UiPathSegment]) -> Option<&'a Value> {
    for segment in segments {
        value = match (segment, value) {
            (UiPathSegment::Key(k), Value::Object(o)) => o.get(k)?,
            (UiPathSegment::Index(i), Value::Array(a)) => a.get(*i)?,
            _ => return None,
        };
    }
    Some(value)
}

/// Estimated memory of `value`, checking depth and size bounds.
fn measure(value: &Value, depth: usize) -> Result<usize> {
    if depth > MAX_DATA_DEPTH {
        return Err(bind_error("Data value nests deeper than 16 levels"));
    }
    Ok(match value {
        Value::Null | Value::Bool(_) => 8,
        Value::Number(_) => 16,
        Value::String(s) => {
            if s.len() > MAX_DATA_STRING {
                return Err(bind_error("Data string longer than 64 KiB"));
            }
            16 + s.len()
        }
        Value::Array(items) => {
            if items.len() > MAX_DATA_ITEMS {
                return Err(bind_error("Data array longer than 1,000,000 items"));
            }
            let mut total = 16;
            for item in items {
                total += measure(item, depth + 1)?;
            }
            total
        }
        Value::Object(map) => {
            if map.len() > MAX_DATA_ITEMS {
                return Err(bind_error("Data object with more than 1,000,000 keys"));
            }
            let mut total = 16;
            for (key, item) in map {
                total += 16 + key.len() + measure(item, depth + 1)?;
            }
            total
        }
    })
}

/// Shared typed data the bindings read and two-way bindings write (resource).
/// Values are JSON; paths use [`parse_path`]. Every effective change bumps
/// [`UiDataContext::version`], which bindings use for change detection.
#[derive(Clone, Debug, Resource)]
pub struct UiDataContext {
    root: Arc<Value>,
    version: u64,
    bytes: usize,
}

impl Default for UiDataContext {
    fn default() -> Self {
        Self {
            root: Arc::new(Value::Object(Map::new())),
            version: 0,
            bytes: 16,
        }
    }
}

impl UiDataContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Context holding `values` (must be a JSON object).
    pub fn from_value(values: Value) -> Result<Self> {
        if !values.is_object() {
            return Err(bind_error("A data context root must be an object"));
        }
        let bytes = measure(&values, 0)?;
        if bytes > MAX_DATA_BYTES {
            return Err(bind_error("Data context exceeds 16 MiB"));
        }
        Ok(Self {
            root: Arc::new(values),
            version: 1,
            bytes,
        })
    }

    /// Increases on every change; unchanged writes do not bump it.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn root(&self) -> &Value {
        &self.root
    }

    pub(super) fn snapshot(&self) -> Arc<Value> {
        self.root.clone()
    }

    pub fn get(&self, path: &str) -> Result<Option<&Value>> {
        Ok(walk(&self.root, &parse_path(path)?))
    }

    pub fn get_f64(&self, path: &str) -> Option<f64> {
        self.get(path).ok().flatten().and_then(Value::as_f64)
    }

    pub fn get_bool(&self, path: &str) -> Option<bool> {
        self.get(path).ok().flatten().and_then(Value::as_bool)
    }

    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get(path).ok().flatten().and_then(Value::as_str)
    }

    /// Write `value` at `path`, creating objects along the way. An array
    /// index may address an existing element or the next free slot. Returns
    /// whether the context changed.
    pub fn set(&mut self, path: &str, value: Value) -> Result<bool> {
        let segments = parse_path(path)?;
        let added = measure(&value, segments.len())?;
        let removed = walk(&self.root, &segments).map_or(0, |v| measure(v, 0).unwrap_or(0));
        if self.bytes - removed.min(self.bytes) + added > MAX_DATA_BYTES {
            return Err(bind_error("Data context would exceed 16 MiB"));
        }
        check_set(&self.root, &segments, path)?;
        if walk(&self.root, &segments) == Some(&value) {
            return Ok(false);
        }
        set_at(Arc::make_mut(&mut self.root), &segments, value);
        self.bytes = self.bytes - removed.min(self.bytes) + added;
        self.version += 1;
        Ok(true)
    }

    /// Set every top-level key of `values`; returns whether anything changed.
    pub fn merge(&mut self, values: &Map<String, Value>) -> Result<bool> {
        let mut changed = false;
        for (key, value) in values {
            changed |= self.set(key, value.clone())?;
        }
        Ok(changed)
    }

    /// Remove the value at `path` (an array element shifts later ones down).
    pub fn remove(&mut self, path: &str) -> Result<bool> {
        let segments = parse_path(path)?;
        let (last, parent) = segments.split_last().expect("paths are non-empty");
        let Some(existing) = walk(&self.root, &segments) else {
            return Ok(false);
        };
        let removed = measure(existing, 0).unwrap_or(0);
        let root = Arc::make_mut(&mut self.root);
        let Some(container) = walk_mut(root, parent) else {
            return Ok(false);
        };
        match (last, container) {
            (UiPathSegment::Key(k), Value::Object(o)) => {
                o.remove(k);
            }
            (UiPathSegment::Index(i), Value::Array(a)) => {
                a.remove(*i);
            }
            _ => return Ok(false),
        }
        self.bytes = self.bytes.saturating_sub(removed);
        self.version += 1;
        Ok(true)
    }

    pub fn clear(&mut self) {
        if self.bytes > 16 || self.root.as_object().is_some_and(|o| !o.is_empty()) {
            self.root = Arc::new(Value::Object(Map::new()));
            self.bytes = 16;
            self.version += 1;
        }
    }
}

fn walk_mut<'a>(mut value: &'a mut Value, segments: &[UiPathSegment]) -> Option<&'a mut Value> {
    for segment in segments {
        value = match (segment, value) {
            (UiPathSegment::Key(k), Value::Object(o)) => o.get_mut(k)?,
            (UiPathSegment::Index(i), Value::Array(a)) => a.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(value)
}

/// Reject writes that would address through a value of the wrong type or past
/// the end of an array, before anything is modified.
fn check_set(root: &Value, segments: &[UiPathSegment], path: &str) -> Result<()> {
    let mut value = Some(root);
    for segment in segments {
        let Some(current) = value else {
            return Ok(());
        };
        value = match (segment, current) {
            (UiPathSegment::Key(k), Value::Object(o)) => {
                if o.len() >= MAX_DATA_ITEMS && !o.contains_key(k) {
                    return Err(path_error(path, "object has too many keys"));
                }
                o.get(k)
            }
            (UiPathSegment::Index(i), Value::Array(a)) => {
                if *i > a.len() || *i >= MAX_DATA_ITEMS {
                    return Err(path_error(path, "array index beyond the next free slot"));
                }
                a.get(*i)
            }
            (_, Value::Null) => None,
            _ => return Err(path_error(path, "crosses a value of the wrong type")),
        };
    }
    Ok(())
}

fn set_at(slot: &mut Value, segments: &[UiPathSegment], new: Value) {
    let Some((first, rest)) = segments.split_first() else {
        *slot = new;
        return;
    };
    match first {
        UiPathSegment::Key(k) => {
            if slot.is_null() {
                *slot = Value::Object(Map::new());
            }
            if let Value::Object(o) = slot {
                set_at(o.entry(k.clone()).or_insert(Value::Null), rest, new);
            }
        }
        UiPathSegment::Index(i) => {
            if slot.is_null() {
                *slot = Value::Array(vec![]);
            }
            if let Value::Array(a) = slot {
                if *i == a.len() {
                    a.push(Value::Null);
                }
                set_at(&mut a[*i], rest, new);
            }
        }
    }
}

/// Message-key resolution seam. A localization layer implements this and
/// installs it with [`UiBindingState::set_resolver`]; bindings with a
/// `message` key then show the resolver's text, falling back to their
/// `format` or value when it returns `None`.
pub trait UiStringResolver: Send + Sync {
    /// Resolve message `key` with named `args` (values read from the data
    /// context at the binding's `args` paths).
    fn resolve(&self, key: &str, args: &BTreeMap<String, Value>) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UiBindProperty {
    /// Label/rich label/button/toggle text or text-input value.
    Text,
    /// Slider or progress value.
    Value,
    /// Toggle state.
    Checked,
    /// Dropdown, list, virtual list or tab selection (`null` clears it).
    Selected,
    Visible,
    Enabled,
    /// Virtual-list item count from an array length or a number.
    ItemCount,
    /// Text of each virtual-list template node, per item.
    ItemText,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiBindMode {
    /// The data drives the widget.
    #[default]
    OneWay,
    /// Input widgets also write user changes back into the data.
    TwoWay,
}

fn is_one_way(mode: &UiBindMode) -> bool {
    *mode == UiBindMode::OneWay
}

/// Connects one property of one node to a data path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UiBinding {
    pub node: String,
    pub property: UiBindProperty,
    /// Data path; for `itemText` the array of items.
    pub path: String,
    #[serde(default, skip_serializing_if = "is_one_way")]
    pub mode: UiBindMode,
    /// Template with `{path}`, `{path:.2}`, `{}` (this binding's value),
    /// `{.field}`/`{#}` for item text, and `{{`/`}}` escapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Message key resolved through [`UiStringResolver`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Named message arguments: name to data path (item-relative for `itemText`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
    /// Path of the text inside each item (`itemText` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
}

impl UiBinding {
    pub fn one_way(
        node: impl Into<String>,
        property: UiBindProperty,
        path: impl Into<String>,
    ) -> Self {
        Self {
            node: node.into(),
            property,
            path: path.into(),
            mode: UiBindMode::OneWay,
            format: None,
            message: None,
            args: BTreeMap::new(),
            item_path: None,
        }
    }

    pub fn two_way(
        node: impl Into<String>,
        property: UiBindProperty,
        path: impl Into<String>,
    ) -> Self {
        Self {
            mode: UiBindMode::TwoWay,
            ..Self::one_way(node, property, path)
        }
    }

    pub fn with_format(mut self, format: impl Into<String>) -> Self {
        self.format = Some(format.into());
        self
    }

    pub fn with_message(
        mut self,
        key: impl Into<String>,
        args: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.message = Some(key.into());
        self.args = args.into_iter().collect();
        self
    }

    pub fn with_item_path(mut self, path: impl Into<String>) -> Self {
        self.item_path = Some(path.into());
        self
    }

    fn validate(&self, nodes: &[UiNode], hierarchy: &Hierarchy) -> Result<()> {
        let Some(&index) = hierarchy.index.get(&self.node) else {
            return Err(bind_error(format!(
                "Binding targets unknown node {}",
                self.node
            )));
        };
        parse_path(&self.path)?;
        let widget = &nodes[index].widget;
        use UiBindProperty as P;
        let compatible = match self.property {
            P::Text => matches!(
                widget,
                UiWidget::Label { .. }
                    | UiWidget::RichLabel { .. }
                    | UiWidget::Button { .. }
                    | UiWidget::Toggle { .. }
                    | UiWidget::TextInput { .. }
            ),
            P::Value => matches!(widget, UiWidget::Slider { .. } | UiWidget::Progress { .. }),
            P::Checked => matches!(widget, UiWidget::Toggle { .. }),
            P::Selected => matches!(
                widget,
                UiWidget::Dropdown { .. }
                    | UiWidget::List { .. }
                    | UiWidget::VirtualList { .. }
                    | UiWidget::Tabs { .. }
            ),
            P::Visible | P::Enabled => true,
            P::ItemCount => matches!(widget, UiWidget::VirtualList { .. }),
            P::ItemText => {
                matches!(
                    widget,
                    UiWidget::Label { .. }
                        | UiWidget::RichLabel { .. }
                        | UiWidget::Button { .. }
                        | UiWidget::Toggle { .. }
                ) && hierarchy
                    .ancestors(index)
                    .any(|a| matches!(nodes[a].widget, UiWidget::VirtualList { .. }))
            }
        };
        if !compatible {
            return Err(bind_error(format!(
                "Binding property {:?} does not fit node {} ({})",
                self.property,
                self.node,
                selector::kind_name(widget)
            )));
        }
        if self.mode == UiBindMode::TwoWay {
            let writable = match self.property {
                P::Text => matches!(widget, UiWidget::TextInput { .. }),
                P::Value => matches!(widget, UiWidget::Slider { .. }),
                P::Checked | P::Selected => true,
                _ => false,
            };
            if !writable || self.format.is_some() || self.message.is_some() || !self.args.is_empty()
            {
                return Err(bind_error(format!(
                    "Two-way binding on node {} needs an input widget, no format and no message",
                    self.node
                )));
            }
        }
        if self.item_path.is_some() && self.property != P::ItemText {
            return Err(bind_error("itemPath is only valid on itemText bindings"));
        }
        if let Some(item_path) = &self.item_path {
            parse_path(item_path)?;
        }
        if let Some(format) = &self.format {
            check_template(format)?;
        }
        if let Some(key) = &self.message
            && (key.is_empty()
                || key.len() > 128
                || !key.chars().all(|c| key_char(c) || matches!(c, '.' | '$')))
        {
            return Err(bind_error("Invalid message key"));
        }
        if self.args.len() > MAX_BIND_ARGS
            || !self.args.iter().all(|(n, p)| {
                !n.is_empty() && n.len() <= 64 && n.chars().all(key_char) && parse_path(p).is_ok()
            })
        {
            return Err(bind_error("Invalid message arguments"));
        }
        Ok(())
    }
}

/// Validate all bindings of a root against its hierarchy.
pub(super) fn validate_all(
    bindings: &[UiBinding],
    nodes: &[UiNode],
    hierarchy: &Hierarchy,
) -> Result<()> {
    if bindings.len() > MAX_UI_BINDINGS {
        return Err(bind_error("UI root exceeds 512 bindings"));
    }
    let mut seen = BTreeSet::new();
    for binding in bindings {
        binding.validate(nodes, hierarchy)?;
        if !seen.insert((binding.node.as_str(), binding.property as u8)) {
            return Err(bind_error(format!(
                "Node {} binds {:?} more than once",
                binding.node, binding.property
            )));
        }
    }
    Ok(())
}

/// Check a format template's braces, paths and precision specs.
fn check_template(template: &str) -> Result<()> {
    if template.len() > MAX_BIND_FORMAT {
        return Err(bind_error("Format longer than 256 bytes"));
    }
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '{' => {
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => inner.push(c),
                        None => return Err(bind_error("Unclosed `{` in format")),
                    }
                }
                let (path, spec) = inner
                    .split_once(':')
                    .map_or((inner.as_str(), None), |(p, s)| (p, Some(s)));
                let path_ok = match path {
                    "" | "#" | "." => true,
                    p => parse_path(p.strip_prefix('.').unwrap_or(p)).is_ok(),
                };
                let spec_ok = spec.is_none_or(|s| {
                    s.strip_prefix('.')
                        .is_some_and(|n| n.len() == 1 && n.chars().all(|d| d.is_ascii_digit()))
                });
                if !path_ok || !spec_ok {
                    return Err(bind_error(format!("Bad format placeholder {{{inner}}}")));
                }
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '}' => return Err(bind_error("Unescaped `}` in format")),
            _ => {}
        }
    }
    Ok(())
}

fn value_text(value: &Value, precision: Option<usize>) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => number_text(n, precision),
        other => {
            let mut text = other.to_string();
            text.truncate(text.floor_char_boundary(MAX_BIND_FORMAT));
            text
        }
    }
}

fn number_text(n: &Number, precision: Option<usize>) -> String {
    let f = n.as_f64().unwrap_or(0.0);
    if let Some(p) = precision {
        return format!("{f:.p$}");
    }
    if n.is_i64() || n.is_u64() {
        return n.to_string();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    format!("{f}")
}

struct TemplateContext<'a> {
    data: &'a Value,
    /// This binding's own value (`{}`).
    primary: Option<&'a Value>,
    /// Current item and its index (`{.field}`, `{#}`).
    item: Option<(&'a Value, u32)>,
}

fn expand(inner: &str, cx: &TemplateContext<'_>) -> String {
    let (path, spec) = inner
        .split_once(':')
        .map_or((inner, None), |(p, s)| (p, Some(s)));
    let precision = spec
        .and_then(|s| s.strip_prefix('.'))
        .and_then(|n| n.parse::<usize>().ok());
    let value = match path {
        "" => cx.primary,
        "#" => return cx.item.map(|(_, i)| i.to_string()).unwrap_or_default(),
        "." => cx.item.map(|(v, _)| v),
        p => match p.strip_prefix('.') {
            Some(relative) => cx
                .item
                .and_then(|(v, _)| parse_path(relative).ok().and_then(|s| walk(v, &s))),
            None => parse_path(p).ok().and_then(|s| walk(cx.data, &s)),
        },
    };
    value.map(|v| value_text(v, precision)).unwrap_or_default()
}

fn render_template(template: &str, cx: &TemplateContext<'_>) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '{' => {
                let mut inner = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    inner.push(c);
                }
                if closed {
                    out.push_str(&expand(&inner, cx));
                } else {
                    out.push('{');
                    out.push_str(&inner);
                }
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            c => out.push(c),
        }
    }
    out
}

impl UiBinding {
    /// The text a `text` or `itemText` binding shows for `primary`.
    fn text(
        &self,
        cx: &TemplateContext<'_>,
        resolver: Option<&dyn UiStringResolver>,
        item: bool,
    ) -> Option<String> {
        if let (Some(key), Some(resolver)) = (&self.message, resolver) {
            let mut args = BTreeMap::new();
            for (name, path) in &self.args {
                let found = parse_path(path).ok().and_then(|s| {
                    if item {
                        cx.item.and_then(|(v, _)| walk(v, &s))
                    } else {
                        walk(cx.data, &s)
                    }
                });
                args.insert(name.clone(), found.cloned().unwrap_or(Value::Null));
            }
            if let Some(text) = resolver.resolve(key, &args) {
                return Some(text);
            }
        }
        if let Some(format) = &self.format {
            return Some(render_template(format, cx));
        }
        cx.primary.map(|v| value_text(v, None))
    }

    /// The normalized value this binding wants its property to have, or `None`
    /// while the data has no value at the binding's `path` yet.
    fn evaluate(&self, data: &Value, resolver: Option<&dyn UiStringResolver>) -> Option<Value> {
        let segments = parse_path(&self.path).ok()?;
        let primary = walk(data, &segments);
        let cx = TemplateContext {
            data,
            primary,
            item: None,
        };
        match self.property {
            UiBindProperty::Text => {
                primary?;
                self.text(&cx, resolver, false).map(Value::String)
            }
            UiBindProperty::Value => {
                let f = primary?.as_f64().filter(|f| f.is_finite())?;
                Number::from_f64(f).map(Value::Number)
            }
            UiBindProperty::Checked | UiBindProperty::Visible | UiBindProperty::Enabled => {
                match primary? {
                    Value::Bool(b) => Some(Value::Bool(*b)),
                    Value::Number(n) => Some(Value::Bool(n.as_f64() != Some(0.0))),
                    _ => None,
                }
            }
            UiBindProperty::Selected => match primary? {
                Value::Null => Some(Value::Null),
                Value::Number(n) => n.as_u64().map(|v| Value::Number(v.into())),
                _ => None,
            },
            UiBindProperty::ItemCount => {
                let count = match primary? {
                    Value::Array(a) => a.len() as u64,
                    Value::Object(o) => o.len() as u64,
                    Value::Number(n) => n.as_u64()?,
                    _ => return None,
                };
                Some(Value::Number(
                    count.min(u64::from(MAX_VIRTUAL_ITEMS)).into(),
                ))
            }
            UiBindProperty::ItemText => None,
        }
    }
}

fn clip_text(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        text.truncate(text.floor_char_boundary(limit));
    }
    text
}

/// Store a normalized binding value into the widget or node.
fn apply_value(node: &mut UiNode, property: UiBindProperty, value: &Value) {
    use UiBindProperty as P;
    match property {
        P::Text => {
            let text = value.as_str().unwrap_or_default().to_owned();
            match &mut node.widget {
                UiWidget::Label { text: t }
                | UiWidget::Button { text: t, .. }
                | UiWidget::Toggle { text: t, .. } => *t = clip_text(text, MAX_UI_TEXT),
                UiWidget::RichLabel { markup } => *markup = clip_text(text, MAX_UI_TEXT),
                UiWidget::TextInput {
                    text: t,
                    max_length,
                    caret,
                    anchor,
                    multiline,
                    ..
                } => {
                    let cleaned: String = text
                        .chars()
                        .map(|c| if c == '\n' && !*multiline { ' ' } else { c })
                        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                        .take(*max_length as usize)
                        .collect();
                    let chars = cleaned.chars().count() as u32;
                    *caret = (*caret).min(chars);
                    *anchor = (*anchor).min(chars);
                    *t = cleaned;
                }
                _ => {}
            }
        }
        P::Value => {
            if let Some(f) = value.as_f64().map(|f| f as f32).filter(|f| f.is_finite()) {
                match &mut node.widget {
                    UiWidget::Slider {
                        value: v, min, max, ..
                    }
                    | UiWidget::Progress {
                        value: v, min, max, ..
                    } => *v = f.clamp(*min, *max),
                    _ => {}
                }
            }
        }
        P::Checked => {
            if let (UiWidget::Toggle { checked, .. }, Some(b)) = (&mut node.widget, value.as_bool())
            {
                *checked = b;
            }
        }
        P::Selected => {
            let index = value.as_u64().map(|v| v.min(u64::from(u32::MAX)) as u32);
            match &mut node.widget {
                UiWidget::Dropdown {
                    selected, options, ..
                } => *selected = index.filter(|i| (*i as usize) < options.len()),
                UiWidget::List { selected, .. } => *selected = index,
                UiWidget::VirtualList {
                    selected,
                    item_count,
                    ..
                } => *selected = index.filter(|i| i < item_count),
                UiWidget::Tabs {
                    selected, labels, ..
                } => {
                    if let Some(i) = index {
                        *selected = i.min(labels.len().saturating_sub(1) as u32);
                    }
                }
                _ => {}
            }
        }
        P::Visible => node.visible = value.as_bool().unwrap_or(node.visible),
        P::Enabled => node.enabled = value.as_bool().unwrap_or(node.enabled),
        P::ItemCount => {
            if let (
                UiWidget::VirtualList {
                    item_count,
                    selected,
                    ..
                },
                Some(count),
            ) = (&mut node.widget, value.as_u64())
            {
                *item_count = count.min(u64::from(MAX_VIRTUAL_ITEMS)) as u32;
                if selected.is_some_and(|s| s >= *item_count) {
                    *selected = None;
                }
            }
        }
        P::ItemText => {}
    }
}

/// The value of a two-way property as it currently stands in the widget.
fn read_widget(node: &UiNode, property: UiBindProperty) -> Option<Value> {
    use UiBindProperty as P;
    match (property, &node.widget) {
        (P::Text, UiWidget::TextInput { text, .. }) => Some(Value::String(text.clone())),
        (P::Value, UiWidget::Slider { value, .. }) => {
            // Shortest decimal text keeps 0.3 as 0.3 instead of 0.30000001192.
            value
                .to_string()
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map(Value::Number)
        }
        (P::Checked, UiWidget::Toggle { checked, .. }) => Some(Value::Bool(*checked)),
        (
            P::Selected,
            UiWidget::Dropdown { selected, .. }
            | UiWidget::List { selected, .. }
            | UiWidget::VirtualList { selected, .. },
        ) => Some(selected.map_or(Value::Null, |s| Value::Number(s.into()))),
        (P::Selected, UiWidget::Tabs { selected, .. }) => Some(Value::Number((*selected).into())),
        _ => None,
    }
}

/// Text of virtual-list template nodes for an item (render-time hook).
pub trait UiItemTextSource: Send + Sync {
    fn text(&self, node: &str, item: u32) -> Option<String>;
}

/// Optional per-item text source carried by [`UiRenderState`].
#[derive(Clone, Default)]
pub struct UiItemTexts(Option<Arc<dyn UiItemTextSource>>);

impl UiItemTexts {
    pub fn new(source: Arc<dyn UiItemTextSource>) -> Self {
        Self(Some(source))
    }
    pub fn text(&self, node: &str, item: u32) -> Option<String> {
        self.0.as_ref()?.text(node, item)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

impl std::fmt::Debug for UiItemTexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "UiItemTexts(source)"
        } else {
            "UiItemTexts(none)"
        })
    }
}

impl PartialEq for UiItemTexts {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

struct DataItemText {
    data: Arc<Value>,
    by_node: BTreeMap<String, UiBinding>,
    resolver: Option<Arc<dyn UiStringResolver>>,
}

impl UiItemTextSource for DataItemText {
    fn text(&self, node: &str, item: u32) -> Option<String> {
        let binding = self.by_node.get(node)?;
        let items = walk(&self.data, &parse_path(&binding.path).ok()?)?;
        let element = items.as_array()?.get(item as usize)?;
        let primary = match &binding.item_path {
            Some(path) => walk(element, &parse_path(path).ok()?),
            None => Some(element),
        };
        let cx = TemplateContext {
            data: &self.data,
            primary,
            item: Some((element, item)),
        };
        binding.text(&cx, self.resolver.as_deref(), true)
    }
}

impl UiNode {
    /// Copy of this node showing `text` (labels, buttons, toggles, rich labels).
    pub(super) fn with_bound_text(&self, text: String) -> Self {
        let mut node = self.clone();
        apply_value(&mut node, UiBindProperty::Text, &Value::String(text));
        node
    }
}

/// Counters of binding work, for tests and profiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiBindingStats {
    /// Root evaluations that re-read the data (the version had changed).
    pub frames_evaluated: u64,
    /// Root evaluations skipped because nothing changed.
    pub frames_skipped: u64,
    /// Bindings whose source was read.
    pub evaluated: u64,
    /// Bindings that changed their widget property.
    pub applied: u64,
    /// Two-way writes into the data context.
    pub writes: u64,
}

#[derive(Debug, Default)]
struct RootBindings {
    entity: u64,
    epoch: u64,
    bindings: Vec<UiBinding>,
    /// Last value applied per binding.
    last: Vec<Option<Value>>,
    /// Last widget value seen per two-way binding.
    seen: Vec<Option<Value>>,
    seen_version: Option<u64>,
}

/// Binding bookkeeping (resource): change detection, the string resolver and
/// statistics.
#[derive(Default, Resource)]
pub struct UiBindingState {
    roots: BTreeMap<String, RootBindings>,
    components: BTreeMap<u64, Map<String, Value>>,
    resolver: Option<Arc<dyn UiStringResolver>>,
    epoch: u64,
    stats: UiBindingStats,
}

impl std::fmt::Debug for UiBindingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiBindingState")
            .field("roots", &self.roots.keys().collect::<Vec<_>>())
            .field("stats", &self.stats)
            .finish()
    }
}

impl UiBindingState {
    pub fn stats(&self) -> UiBindingStats {
        self.stats
    }

    /// Install the message-key resolver and re-render every message binding.
    pub fn set_resolver(&mut self, resolver: Arc<dyn UiStringResolver>) {
        self.resolver = Some(resolver);
        self.epoch += 1;
    }

    pub fn clear_resolver(&mut self) {
        self.resolver = None;
        self.epoch += 1;
    }

    /// Re-render every binding on the next tick (call after the locale changes).
    pub fn invalidate_strings(&mut self) {
        self.epoch += 1;
    }

    /// Forget caches (scene change); the next tick re-applies every binding.
    pub fn reset(&mut self) {
        self.roots.clear();
        self.components.clear();
    }

    pub(super) fn retain_roots(&mut self, ids: &BTreeSet<String>) {
        self.roots.retain(|id, _| ids.contains(id));
    }

    /// Merge changed `engine.UiData` components into `data`.
    pub(super) fn sync_components(
        &mut self,
        world: &World,
        data: &mut UiDataContext,
    ) -> Result<()> {
        let mut sources: Vec<(u64, &UiData)> = world
            .iter_entities()
            .filter_map(|e| e.get::<UiData>().map(|d| (e.id().to_bits(), d)))
            .collect();
        sources.sort_by_key(|(key, _)| *key);
        let live: BTreeSet<u64> = sources.iter().map(|(key, _)| *key).collect();
        for (key, component) in sources {
            if self.components.get(&key) != Some(&component.values) {
                data.merge(&component.values)?;
                self.components.insert(key, component.values.clone());
            }
        }
        self.components.retain(|key, _| live.contains(key));
        Ok(())
    }

    /// One-way pass: write changed source values into `root`'s widgets.
    pub(super) fn apply(&mut self, id: &str, entity: u64, root: &mut UiRoot, data: &UiDataContext) {
        let Self {
            roots,
            resolver,
            epoch,
            stats,
            ..
        } = self;
        if root.bindings.is_empty() {
            roots.remove(id);
            return;
        }
        let state = roots.entry(id.to_owned()).or_default();
        if state.entity != entity || state.epoch != *epoch || state.bindings != root.bindings {
            let n = root.bindings.len();
            *state = RootBindings {
                entity,
                epoch: *epoch,
                bindings: root.bindings.clone(),
                last: vec![None; n],
                seen: vec![None; n],
                seen_version: None,
            };
        }
        if state.seen_version == Some(data.version) {
            stats.frames_skipped += 1;
            return;
        }
        stats.frames_evaluated += 1;
        let index: BTreeMap<&str, usize> = root
            .tree
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let targets: Vec<Option<usize>> = root
            .bindings
            .iter()
            .map(|b| index.get(b.node.as_str()).copied())
            .collect();
        let resolver = resolver.as_deref();
        for (i, binding) in root.bindings.iter().enumerate() {
            let Some(node) = targets[i] else {
                continue;
            };
            if binding.property == UiBindProperty::ItemText {
                continue;
            }
            stats.evaluated += 1;
            let source = binding.evaluate(&data.root, resolver);
            let node = &mut root.tree.nodes[node];
            match source {
                Some(value) if state.last[i].as_ref() != Some(&value) => {
                    apply_value(node, binding.property, &value);
                    state.last[i] = Some(value);
                    stats.applied += 1;
                    if binding.mode == UiBindMode::TwoWay {
                        state.seen[i] = read_widget(node, binding.property);
                    }
                }
                None if binding.mode == UiBindMode::TwoWay && state.seen[i].is_none() => {
                    state.seen[i] = read_widget(node, binding.property);
                }
                _ => {}
            }
        }
        state.seen_version = Some(data.version);
    }

    /// Two-way pass: write widget changes made since the last pass into `data`.
    pub(super) fn write_back(
        &mut self,
        id: &str,
        root: &UiRoot,
        data: &mut UiDataContext,
    ) -> Result<()> {
        let Self { roots, stats, .. } = self;
        let Some(state) = roots.get_mut(id) else {
            return Ok(());
        };
        if state.bindings != root.bindings {
            return Ok(());
        }
        let before = data.version;
        let mut wrote = false;
        for (i, binding) in root.bindings.iter().enumerate() {
            if binding.mode != UiBindMode::TwoWay {
                continue;
            }
            let Some(node) = root.tree.find(&binding.node) else {
                continue;
            };
            let Some(current) = read_widget(&root.tree.nodes[node], binding.property) else {
                continue;
            };
            match &state.seen[i] {
                None => state.seen[i] = Some(current),
                Some(previous) if *previous != current => {
                    data.set(&binding.path, current.clone())?;
                    stats.writes += 1;
                    state.seen[i] = Some(current.clone());
                    state.last[i] = Some(current);
                    wrote = true;
                }
                Some(_) => {}
            }
        }
        // Our own writes must not be re-applied next tick, but changes made
        // by anyone else in between must still be.
        if wrote && state.seen_version == Some(before) && data.version >= before {
            state.seen_version = Some(data.version);
        }
        Ok(())
    }

    /// Per-item text source for `root`, if it has `itemText` bindings.
    pub fn item_texts(&self, root: &UiRoot, data: &UiDataContext) -> UiItemTexts {
        let by_node: BTreeMap<String, UiBinding> = root
            .bindings
            .iter()
            .filter(|b| b.property == UiBindProperty::ItemText)
            .map(|b| (b.node.clone(), b.clone()))
            .collect();
        if by_node.is_empty() {
            return UiItemTexts::default();
        }
        UiItemTexts::new(Arc::new(DataItemText {
            data: data.snapshot(),
            by_node,
            resolver: self.resolver.clone(),
        }))
    }
}

/// Authored initial and driven data (`engine.UiData`): top-level keys merge
/// into the [`UiDataContext`] whenever the component changes.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiData {
    pub version: u32,
    pub values: Map<String, Value>,
}

impl Default for UiData {
    fn default() -> Self {
        Self {
            version: 1,
            values: Map::new(),
        }
    }
}

impl UiData {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(bind_error("Unsupported UiData version"));
        }
        if self.values.len() > 256
            || !self
                .values
                .keys()
                .all(|k| parse_path(k).is_ok_and(|s| s.len() == 1))
        {
            return Err(bind_error(
                "UiData has at most 256 keys named with [A-Za-z0-9_-]",
            ));
        }
        self.values
            .values()
            .try_for_each(|v| measure(v, 0).map(|_| ()))
    }
}
