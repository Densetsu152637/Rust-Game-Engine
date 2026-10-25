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

/// Invisible Unicode format characters (general category Cf, plus the line and paragraph
/// separators) that can reorder or hide text: bidirectional embeddings, overrides, isolates and
/// marks, zero-width spaces and joiners, word joiners and invisible operators, the byte order
/// mark, the soft hyphen, interlinear annotation anchors, shorthand and musical format
/// controls, and the tag block. A session name containing them can make a lobby entry display
/// as something else (a spoofed name, text that visually swallows its neighbours).
pub(crate) fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// Characters never allowed in display text.
fn forbidden(c: char) -> bool {
    c.is_control() || is_invisible_format(c)
}

/// Display text: nonempty, bounded, no control characters and no invisible format characters.
pub(crate) fn valid_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(forbidden)
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
            valid_ident(key, max_key) && value.len() <= max_value && !value.chars().any(forbidden)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_rejects_controls_and_invisible_format_characters() {
        assert!(valid_text("Friday night — 日本語 🎮", 64));
        assert!(valid_text("plain", 64));
        assert!(!valid_text("", 64));
        assert!(!valid_text("too long", 3));
        for bad in [
            "a\nb",
            "a\u{0}b",
            // Bidirectional overrides, embeddings, isolates and marks.
            "admin\u{202E}gnp.exe",
            "a\u{202A}b",
            "a\u{202D}b",
            "a\u{2066}b\u{2069}",
            "a\u{200E}b",
            "a\u{200F}b",
            "a\u{061C}b",
            // Zero-width characters.
            "a\u{200B}b",
            "a\u{200C}b",
            "a\u{200D}b",
            "a\u{2060}b",
            "a\u{FEFF}b",
            "a\u{00AD}b",
            "a\u{2028}b",
            "a\u{E0041}b",
        ] {
            assert!(!valid_text(bad, 64), "{bad:?}");
        }
    }

    #[test]
    fn attribute_values_apply_the_same_rule() {
        let ok = BTreeMap::from([("map".to_owned(), "dust".to_owned())]);
        assert!(valid_attributes(&ok, 4, 24, 64));
        let bidi = BTreeMap::from([("map".to_owned(), "du\u{202E}st".to_owned())]);
        assert!(!valid_attributes(&bidi, 4, 24, 64));
        let zero_width = BTreeMap::from([("map".to_owned(), "du\u{200B}st".to_owned())]);
        assert!(!valid_attributes(&zero_width, 4, 24, 64));
    }
}
