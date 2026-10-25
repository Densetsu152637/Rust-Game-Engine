//! Shared string validation. Every identifier that crosses a trust boundary is checked here.
use std::collections::BTreeMap;

/// ASCII identifier: alphanumerics plus `.`, `_`, `-`, `:` and `/`.
pub(crate) fn valid_ident(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'))
}

/// Display text: nonempty, bounded, no control characters.
pub(crate) fn valid_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

/// Attribute map bounds shared by sessions, tickets and filters.
pub(crate) fn valid_attributes(
    attributes: &BTreeMap<String, String>,
    max_entries: usize,
    max_key: usize,
    max_value: usize,
) -> bool {
    attributes.len() <= max_entries
        && attributes.iter().all(|(key, value)| {
            valid_ident(key, max_key)
                && value.len() <= max_value
                && !value.chars().any(char::is_control)
        })
}
