//! Message arguments.

use std::borrow::Cow;

use fluent_bundle::{FluentArgs, FluentValue};

/// One argument value.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgValue {
    Text(String),
    Number(f64),
}

/// Named variables passed to [`crate::Localizer::format`]. Dates are not a built-in type: pass a
/// pre-formatted string (or a number when only plural selection matters).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocaleArgs {
    entries: Vec<(String, ArgValue)>,
}

impl LocaleArgs {
    /// Empty argument set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a string variable.
    pub fn text(mut self, name: &str, value: impl Into<String>) -> Self {
        self.set(name, ArgValue::Text(value.into()));
        self
    }

    /// Adds (or replaces) a numeric variable; used by plural selectors and `NUMBER()`.
    pub fn number(mut self, name: &str, value: f64) -> Self {
        self.set(name, ArgValue::Number(value));
        self
    }

    /// Adds (or replaces) a variable.
    pub fn set(&mut self, name: &str, value: ArgValue) {
        if let Some(slot) = self.entries.iter_mut().find(|(n, _)| n == name) {
            slot.1 = value;
        } else {
            self.entries.push((name.to_string(), value));
        }
    }

    /// Variables in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ArgValue)> {
        self.entries.iter().map(|(n, v)| (n.as_str(), v))
    }

    pub(crate) fn to_fluent(&self, max_arg_bytes: usize) -> FluentArgs<'_> {
        let mut out = FluentArgs::new();
        for (name, value) in &self.entries {
            let v = match value {
                ArgValue::Text(s) => FluentValue::String(Cow::Borrowed(truncate(s, max_arg_bytes))),
                ArgValue::Number(n) if n.is_finite() => FluentValue::from(*n),
                ArgValue::Number(n) => FluentValue::String(Cow::Owned(n.to_string())),
            };
            out.set(name.as_str(), v);
        }
        out
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
