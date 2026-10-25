#![allow(dead_code)]

use engine_text::{
    FaceId, FallbackConfig, FontLibrary, LayoutOptions, RichText, TextLayout, TextLimits, TextStyle,
};

pub const UBUNTU: usize = 0;
pub const HACK: usize = 1;
pub const EMOJI: usize = 2;

/// Ubuntu Light (face 0), Hack (face 1), Noto Emoji (face 2); default fallback: Noto Emoji.
pub fn library() -> FontLibrary {
    library_with_limits(TextLimits::default())
}

pub fn library_with_limits(limits: TextLimits) -> FontLibrary {
    let mut library = FontLibrary::with_limits(limits);
    for bytes in [
        epaint_default_fonts::UBUNTU_LIGHT,
        epaint_default_fonts::HACK_REGULAR,
        epaint_default_fonts::NOTO_EMOJI_REGULAR,
    ] {
        library
            .load_font(bytes.to_vec())
            .expect("bundled test font loads");
    }
    library
        .set_fallback(FallbackConfig::new().with_default(["Noto Emoji"]))
        .expect("fallback fits");
    library
}

pub fn face(library: &FontLibrary, index: usize) -> FaceId {
    library.faces().nth(index).expect("face exists").0
}

pub fn style() -> TextStyle {
    TextStyle::new("Ubuntu", 32.0)
}

pub fn layout(library: &FontLibrary, text: &str, options: &LayoutOptions) -> TextLayout {
    library
        .layout(&RichText::plain(text), &style(), options)
        .expect("layout succeeds")
}

pub fn wrapped(width: f32) -> LayoutOptions {
    LayoutOptions {
        max_width: Some(width),
        ..LayoutOptions::default()
    }
}

/// Glyphs whose cluster starts at `byte`.
pub fn glyph_x_at(layout: &TextLayout, byte: usize) -> f32 {
    layout
        .glyphs
        .iter()
        .find(|g| g.cluster.start == byte)
        .unwrap_or_else(|| panic!("no glyph for byte {byte}"))
        .x
}
