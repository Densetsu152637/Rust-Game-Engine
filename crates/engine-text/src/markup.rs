//! Bounded BBCode-style markup parsed into [`RichText`] through [`RichTextBuilder`].
//!
//! Grammar (tags are case-sensitive, lowercase):
//!
//! | Tag | Meaning |
//! | --- | --- |
//! | `[b]..[/b]` | bold |
//! | `[i]..[/i]` | italic |
//! | `[u]..[/u]` | underline |
//! | `[color=#rgb\|#rrggbb\|#rrggbbaa]..[/color]` | colour |
//! | `[size=N]..[/size]` | size in pixels |
//! | `[font=Family Name]..[/font]` | family |
//! | `[link=id]..[/link]` | link region id (`A-Z a-z 0-9 _ . : / # -`) |
//! | `[img=key width=W height=H]` | inline image placeholder (self-closing) |
//!
//! Escapes: `\[`, `\]` and `\\`; any other backslash is literal.

use crate::rich::{MAX_KEY_BYTES, valid_image_edge};
use crate::{
    Color, MarkupErrorKind, RichText, RichTextBuilder, StyleChange, TextError, TextLimits,
};

/// What to do with a tag name outside the vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnknownTagPolicy {
    /// Fail with `text.markup.unknown_tag` (default; authored content).
    #[default]
    Error,
    /// Keep the bracketed text verbatim, including an unterminated `[` (player-typed text).
    Literal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MarkupOptions {
    pub unknown_tags: UnknownTagPolicy,
    pub limits: TextLimits,
}

/// Longest tag (between the brackets) in bytes.
const MAX_TAG_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TagName {
    Bold,
    Italic,
    Underline,
    Color,
    Size,
    Font,
    Link,
}

impl TagName {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "b" => Self::Bold,
            "i" => Self::Italic,
            "u" => Self::Underline,
            "color" => Self::Color,
            "size" => Self::Size,
            "font" => Self::Font,
            "link" => Self::Link,
            _ => return None,
        })
    }
}

/// Parsed content of one bracketed tag.
enum Tag {
    Open(TagName, StyleChange),
    Close(TagName),
    Image {
        key: String,
        width: f32,
        height: f32,
    },
}

impl RichText {
    /// Parses markup with the given options.
    pub fn from_markup(source: &str, options: &MarkupOptions) -> Result<RichText, TextError> {
        parse_markup(source, options)
    }
}

/// Parses markup into rich text. See the module docs for the grammar.
pub fn parse_markup(source: &str, options: &MarkupOptions) -> Result<RichText, TextError> {
    let limits = &options.limits;
    if source.len() > limits.max_text_bytes {
        return Err(TextError::TextTooLong {
            len: source.len(),
            max: limits.max_text_bytes,
        });
    }
    let mut builder = RichTextBuilder::new();
    let mut open: Vec<(TagName, usize)> = Vec::new();
    let mut literal = String::new();
    let mut tags = 0usize;
    let mut rest = source;
    while let Some(c) = rest.chars().next() {
        let offset = source.len() - rest.len();
        match c {
            '\\' => {
                let escaped = rest[1..]
                    .chars()
                    .next()
                    .filter(|n| matches!(n, '[' | ']' | '\\'));
                match escaped {
                    Some(n) => {
                        literal.push(n);
                        rest = &rest[1 + n.len_utf8()..];
                    }
                    None => {
                        literal.push('\\');
                        rest = &rest[1..];
                    }
                }
            }
            '[' => {
                // Look at most one tag length ahead: scanning the whole remainder makes a long
                // run of unterminated `[` quadratic.
                let mut window = (MAX_TAG_BYTES + 1).min(rest.len() - 1);
                while !rest[1..].is_char_boundary(window) {
                    window -= 1;
                }
                let close = rest[1..1 + window].find(']');
                let Some(close) = close else {
                    if options.unknown_tags == UnknownTagPolicy::Literal {
                        literal.push('[');
                        rest = &rest[1..];
                        continue;
                    }
                    return Err(TextError::markup(
                        MarkupErrorKind::Unterminated,
                        offset,
                        "`[` without a closing `]` within 512 bytes",
                    ));
                };
                let content = &rest[1..1 + close];
                let whole = &rest[..close + 2];
                rest = &rest[close + 2..];
                let tag = match parse_tag(content, offset, limits)? {
                    Some(tag) => tag,
                    None if options.unknown_tags == UnknownTagPolicy::Literal => {
                        literal.push_str(whole);
                        continue;
                    }
                    None => {
                        return Err(TextError::markup(
                            MarkupErrorKind::UnknownTag,
                            offset,
                            format!("unknown tag `{whole}`"),
                        ));
                    }
                };
                tags += 1;
                if tags > limits.max_spans {
                    return Err(TextError::TooManySpans {
                        count: tags,
                        max: limits.max_spans,
                    });
                }
                builder.text(&std::mem::take(&mut literal));
                apply_tag(&mut builder, &mut open, tag, offset, limits)?;
            }
            _ => {
                literal.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    if let Some((_, offset)) = open.last() {
        return Err(TextError::markup(
            MarkupErrorKind::Unclosed,
            *offset,
            "tag is never closed",
        ));
    }
    builder.text(&literal);
    let rich = builder.build();
    rich.validate(limits)?;
    Ok(rich)
}

fn apply_tag(
    builder: &mut RichTextBuilder,
    open: &mut Vec<(TagName, usize)>,
    tag: Tag,
    offset: usize,
    limits: &TextLimits,
) -> Result<(), TextError> {
    match tag {
        Tag::Open(name, change) => {
            if open.len() >= limits.max_markup_depth {
                return Err(TextError::markup(
                    MarkupErrorKind::TooDeep,
                    offset,
                    format!("nesting deeper than {}", limits.max_markup_depth),
                ));
            }
            open.push((name, offset));
            builder.push_style(change);
        }
        Tag::Close(name) => {
            match open.last() {
                Some((top, _)) if *top == name => {}
                _ => {
                    return Err(TextError::markup(
                        MarkupErrorKind::Mismatched,
                        offset,
                        "closing tag does not match the innermost open tag",
                    ));
                }
            }
            open.pop();
            builder.pop();
        }
        Tag::Image { key, width, height } => {
            builder.image(key, width, height);
        }
    }
    // Text bytes and spans only grow by bounded amounts per tag; re-check cheaply.
    if builder.text_len() > limits.max_text_bytes {
        return Err(TextError::TextTooLong {
            len: builder.text_len(),
            max: limits.max_text_bytes,
        });
    }
    if builder.span_count() > limits.max_spans {
        return Err(TextError::TooManySpans {
            count: builder.span_count(),
            max: limits.max_spans,
        });
    }
    Ok(())
}

/// Returns `Ok(None)` for an unknown tag name (handled by the policy).
fn parse_tag(content: &str, offset: usize, limits: &TextLimits) -> Result<Option<Tag>, TextError> {
    let bad = |message: String| TextError::markup(MarkupErrorKind::BadValue, offset, message);
    if let Some(name) = content.strip_prefix('/') {
        return Ok(TagName::parse(name).map(Tag::Close));
    }
    let (name, value) = match content.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (content, None),
    };
    if name == "img" {
        return parse_image(value, offset).map(Some);
    }
    let Some(tag) = TagName::parse(name) else {
        return Ok(None);
    };
    let change = match (tag, value) {
        (TagName::Bold, None) => StyleChange::Bold,
        (TagName::Italic, None) => StyleChange::Italic,
        (TagName::Underline, None) => StyleChange::Underline,
        (TagName::Bold | TagName::Italic | TagName::Underline, Some(_)) => {
            return Err(bad(format!("`{name}` takes no value")));
        }
        (_, None) => return Err(bad(format!("`{name}` requires a value"))),
        (TagName::Color, Some(v)) => {
            StyleChange::Color(Color::from_hex(v).ok_or_else(|| bad(format!("bad colour `{v}`")))?)
        }
        (TagName::Size, Some(v)) => {
            let size: f32 = v.parse().map_err(|_| bad(format!("bad size `{v}`")))?;
            limits.check_size(size).map_err(|_| {
                bad(format!(
                    "size {v} outside {}..={} px",
                    limits.min_font_px, limits.max_font_px
                ))
            })?;
            StyleChange::Size(size)
        }
        (TagName::Font, Some(v)) => {
            let family = v.trim();
            if family.is_empty() || family.len() > MAX_KEY_BYTES {
                return Err(bad("font family must be 1..=256 bytes".into()));
            }
            StyleChange::Family(family.to_string())
        }
        (TagName::Link, Some(v)) => {
            if !valid_key(v) {
                return Err(bad(format!("bad link id `{v}`")));
            }
            StyleChange::Link(v.to_string())
        }
    };
    Ok(Some(Tag::Open(tag, change)))
}

fn parse_image(value: Option<&str>, offset: usize) -> Result<Tag, TextError> {
    let bad = |message: String| TextError::markup(MarkupErrorKind::BadValue, offset, message);
    let mut words = value.unwrap_or("").split_whitespace();
    let key = words.next().unwrap_or("");
    if !valid_key(key) {
        return Err(bad(format!("bad image key `{key}`")));
    }
    let (mut width, mut height) = (None, None);
    for word in words {
        let (attr, raw) = word
            .split_once('=')
            .ok_or_else(|| bad(format!("bad image attribute `{word}`")))?;
        let number: f32 = raw
            .parse()
            .ok()
            .filter(|n| valid_image_edge(*n))
            .ok_or_else(|| bad(format!("image {attr} must be in (0, 4096] px")))?;
        match attr {
            "width" => width = Some(number),
            "height" => height = Some(number),
            _ => return Err(bad(format!("unknown image attribute `{attr}`"))),
        }
    }
    match (width, height) {
        (Some(width), Some(height)) => Ok(Tag::Image {
            key: key.to_string(),
            width,
            height,
        }),
        _ => Err(bad("image requires width and height".into())),
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:/#-".contains(&b))
}
