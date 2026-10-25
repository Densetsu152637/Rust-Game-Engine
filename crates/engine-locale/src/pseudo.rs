//! Pseudo-localization: accent substitution, length expansion, markers and RTL mirroring.
//!
//! Runs over `.ftl` values or formatted text. Anything that must survive untouched is copied
//! verbatim: Fluent placeables (`{ $var }`, `{ -term }`, selectors, balanced and string-aware),
//! engine-text markup tags (`[b]`, `[/b]`, `[color=#fff]`, `[img=key width=1 height=1]`) and the
//! escapes `\[`, `\]`, `\\`.

use serde::{Deserialize, Serialize};

use crate::error::LocaleError;

/// Largest accepted `expansion_percent`.
pub const MAX_EXPANSION_PERCENT: u32 = 300;
/// Longest accepted marker, in chars.
pub const MAX_MARKER_CHARS: usize = 8;
const MAX_TAG_BYTES: usize = 512;

const RLO: char = '\u{202E}';
const PDF: char = '\u{202C}';

/// Pseudo-localization settings (serde-friendly; part of the authored settings shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PseudoOptions {
    /// Master switch used by [`crate::Localizer`]; [`pseudo_localize`] itself ignores it.
    pub enabled: bool,
    /// Replace ASCII letters with accented look-alikes.
    pub accents: bool,
    /// Extra length, as a percentage of the translatable character count (0..=300).
    pub expansion_percent: u32,
    /// Text placed before the result. Default `«`. Not `[`, which would collide with markup.
    pub prefix: String,
    /// Text placed after the result. Default `»`.
    pub suffix: String,
    /// Mirror mode: wrap the whole result in RLO ... PDF so it renders right-to-left.
    pub rtl: bool,
}

impl Default for PseudoOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            accents: true,
            expansion_percent: 35,
            prefix: "\u{AB}".to_string(),
            suffix: "\u{BB}".to_string(),
            rtl: false,
        }
    }
}

impl PseudoOptions {
    /// Checks ranges.
    pub fn validate(&self) -> Result<(), LocaleError> {
        let bad = |reason: String| Err(LocaleError::InvalidPseudo { reason });
        if self.expansion_percent > MAX_EXPANSION_PERCENT {
            return bad(format!(
                "expansion_percent {} exceeds {MAX_EXPANSION_PERCENT}",
                self.expansion_percent
            ));
        }
        for (name, m) in [("prefix", &self.prefix), ("suffix", &self.suffix)] {
            if m.chars().count() > MAX_MARKER_CHARS {
                return bad(format!("{name} longer than {MAX_MARKER_CHARS} chars"));
            }
        }
        Ok(())
    }
}

const LOWER: [char; 26] = [
    'á', 'ƀ', 'ç', 'ď', 'é', 'ƒ', 'ĝ', 'ĥ', 'í', 'ĵ', 'ķ', 'ļ', 'ḿ', 'ñ', 'ó', 'þ', 'ǫ', 'ŕ', 'š',
    'ţ', 'ú', 'ṽ', 'ŵ', 'ẋ', 'ý', 'ž',
];
const UPPER: [char; 26] = [
    'Á', 'Ɓ', 'Ç', 'Ď', 'É', 'Ƒ', 'Ĝ', 'Ĥ', 'Í', 'Ĵ', 'Ķ', 'Ļ', 'Ḿ', 'Ñ', 'Ó', 'Þ', 'Ǫ', 'Ŕ', 'Š',
    'Ţ', 'Ú', 'Ṽ', 'Ŵ', 'Ẋ', 'Ý', 'Ž',
];

fn accent(c: char) -> char {
    match c {
        'a'..='z' => LOWER[(c as u8 - b'a') as usize],
        'A'..='Z' => UPPER[(c as u8 - b'A') as usize],
        _ => c,
    }
}

/// Pseudo-localizes `text`. Never panics; options out of range are clamped.
///
/// The output is `[RLO] prefix body filler suffix [PDF]` where `filler` is
/// `ceil(translatable_chars * expansion_percent / 100)` `~` characters appended after the last
/// text, so the translatable length grows by exactly that many chars. Placeables and tags are
/// copied verbatim and do not count toward the length.
pub fn pseudo_localize(text: &str, options: &PseudoOptions) -> String {
    let percent = options.expansion_percent.min(MAX_EXPANSION_PERCENT) as usize;
    let mut body = String::with_capacity(text.len() + text.len() / 2 + 16);
    let mut translatable = 0usize;
    let mut i = 0usize;
    while i < text.len() {
        let rest = &text[i..];
        let Some(c) = rest.chars().next() else { break };
        let keep = match c {
            '\\' => escape_len(rest),
            '{' => placeable_len(rest),
            '[' => tag_len(rest),
            _ => None,
        };
        if let Some(n) = keep {
            body.push_str(&rest[..n]);
            i += n;
            continue;
        }
        if !c.is_control() {
            translatable += 1;
        }
        body.push(if options.accents { accent(c) } else { c });
        i += c.len_utf8();
    }
    if translatable > 0 {
        let extra = (translatable * percent).div_ceil(100);
        body.extend(std::iter::repeat_n('~', extra));
    }
    let mut out = String::with_capacity(body.len() + 16);
    if options.rtl {
        out.push(RLO);
    }
    out.push_str(&options.prefix);
    out.push_str(&body);
    out.push_str(&options.suffix);
    if options.rtl {
        out.push(PDF);
    }
    out
}

fn escape_len(rest: &str) -> Option<usize> {
    let mut it = rest.chars();
    it.next()?;
    match it.next() {
        Some('[' | ']' | '\\') => Some(2),
        _ => None,
    }
}

/// Length in bytes of a balanced `{ ... }` placeable at the start of `rest`, string-aware.
fn placeable_len(rest: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, c) in rest.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Length of a markup-tag-looking `[...]` at the start of `rest`: optional `/`, an ASCII letter,
/// no nested `[`, closed within 512 bytes.
fn tag_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let first = *bytes.get(1)?;
    let name_start = if first == b'/' { 2 } else { 1 };
    if !bytes.get(name_start)?.is_ascii_alphabetic() {
        return None;
    }
    let window = &bytes[..bytes.len().min(MAX_TAG_BYTES)];
    for (idx, b) in window.iter().enumerate().skip(1) {
        match b {
            b'[' => return None,
            b']' => return Some(idx + 1),
            _ => {}
        }
    }
    None
}
