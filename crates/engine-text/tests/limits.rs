mod common;

use common::*;
use engine_text::{
    FontLibrary, LayoutOptions, LineHeight, MarkupOptions, RichText, Span, SpanStyle, TextLimits,
    TextStyle, UnknownTagPolicy, parse_markup,
};

fn code(result: Result<impl std::fmt::Debug, engine_text::TextError>) -> &'static str {
    result.unwrap_err().code()
}

#[test]
fn hostile_fonts_fail_with_typed_errors() {
    let mut library = FontLibrary::new();
    assert_eq!(code(library.load_font(Vec::new())), "text.font.invalid");
    assert_eq!(
        code(library.load_font(vec![0xAB; 4096])),
        "text.font.invalid"
    );
    let mut truncated = epaint_default_fonts::HACK_REGULAR.to_vec();
    truncated.truncate(200);
    assert_eq!(code(library.load_font(truncated)), "text.font.invalid");
    assert_eq!(library.face_count(), 0, "failed loads register nothing");

    let mut small = FontLibrary::with_limits(TextLimits {
        max_font_bytes: 1024,
        ..TextLimits::default()
    });
    assert_eq!(
        code(small.load_font(epaint_default_fonts::HACK_REGULAR.to_vec())),
        "text.font.too_large"
    );
    let mut one = FontLibrary::with_limits(TextLimits {
        max_faces: 1,
        ..TextLimits::default()
    });
    one.load_font(epaint_default_fonts::HACK_REGULAR.to_vec())
        .unwrap();
    assert_eq!(
        code(one.load_font(epaint_default_fonts::HACK_REGULAR.to_vec())),
        "text.font.limit"
    );
}

#[test]
fn layout_without_fonts_reports_none() {
    let library = FontLibrary::new();
    let result = library.layout(&RichText::plain("x"), &style(), &LayoutOptions::default());
    assert_eq!(code(result), "text.font.none");
}

#[test]
fn text_span_glyph_and_size_bounds() {
    let library = library_with_limits(TextLimits {
        max_text_bytes: 32,
        max_spans: 2,
        max_glyphs: 10,
        ..TextLimits::default()
    });
    let lay =
        |rich: &RichText, style: &TextStyle| library.layout(rich, style, &LayoutOptions::default());
    assert_eq!(
        code(lay(&RichText::plain("x".repeat(33)), &style())),
        "text.input.too_long"
    );
    assert_eq!(
        code(lay(&RichText::plain("x".repeat(20)), &style())),
        "text.input.too_many_glyphs"
    );
    let spans = RichText {
        text: "abc".into(),
        spans: (0..3)
            .map(|i| Span {
                range: i..i + 1,
                style: SpanStyle::default(),
            })
            .collect(),
    };
    assert_eq!(code(lay(&spans, &style())), "text.input.too_many_spans");
    for size in [0.0, -4.0, f32::NAN, f32::INFINITY, 4096.0] {
        assert_eq!(
            code(lay(&RichText::plain("x"), &TextStyle::new("Ubuntu", size))),
            "text.input.size_out_of_range"
        );
    }
}

#[test]
fn invalid_spans_and_options_are_rejected() {
    let library = library();
    let bad_spans = [
        vec![Span {
            range: 0..9,
            style: SpanStyle::default(),
        }],
        vec![Span {
            range: 1..2,
            style: SpanStyle::default(),
        }],
        vec![Span {
            range: 2..2,
            style: SpanStyle::default(),
        }],
        vec![
            Span {
                range: 0..3,
                style: SpanStyle::default(),
            },
            Span {
                range: 2..4,
                style: SpanStyle::default(),
            },
        ],
    ];
    for spans in bad_spans {
        let rich = RichText {
            text: "é€x".into(),
            spans,
        };
        assert_eq!(
            code(library.layout(&rich, &style(), &LayoutOptions::default())),
            "text.input.invalid_span"
        );
    }
    for options in [
        LayoutOptions {
            max_width: Some(f32::NAN),
            ..LayoutOptions::default()
        },
        LayoutOptions {
            max_width: Some(-1.0),
            ..LayoutOptions::default()
        },
        LayoutOptions {
            line_height: LineHeight::Absolute(0.0),
            ..LayoutOptions::default()
        },
        LayoutOptions {
            max_lines: Some(0),
            ..LayoutOptions::default()
        },
    ] {
        assert_eq!(
            code(library.layout(&RichText::plain("x"), &style(), &options)),
            "text.layout.invalid_option"
        );
    }
}

/// Deterministic pseudo-random input through markup, layout and caret queries never panics.
#[test]
fn pseudo_random_input_never_panics() {
    let library = library();
    let alphabet: Vec<char> = "ab [/]=#\\ \n\t\u{5D0}\u{627}\u{301}\u{200D}\u{1F600}\u{FFFC}fi\r"
        .chars()
        .chain("[b][/b][img=x width=4 height=4][link=a]".chars())
        .collect();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let options = MarkupOptions {
        unknown_tags: UnknownTagPolicy::Literal,
        ..MarkupOptions::default()
    };
    for round in 0..300 {
        let len = (next() % 60) as usize;
        let source: String = (0..len)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let Ok(rich) = parse_markup(&source, &options) else {
            continue;
        };
        let layout_options = LayoutOptions {
            max_width: Some((next() % 200) as f32),
            max_lines: Some(1 + (next() % 3) as usize),
            overflow: engine_text::Overflow::Ellipsis,
            ..LayoutOptions::default()
        };
        let l = library
            .layout(&rich, &style(), &layout_options)
            .unwrap_or_else(|e| {
                panic!("round {round} {source:?}: {e}");
            });
        for i in 0..=rich.text.len() {
            let r = l.caret_rect(i);
            assert!(r.x.is_finite() && r.y.is_finite());
            let _ = l.hit_test(r.center());
            let _ = l.selection_rects(0..i);
            let _ = l.move_caret(i, engine_text::CaretMove::VisualRight);
            let _ = l.move_caret(i, engine_text::CaretMove::NextWord);
        }
    }
}

/// Near-limit input stays roughly linear: 60 KB of wrapped text with caret queries.
#[test]
fn large_text_layout_and_caret_queries_finish() {
    let library = library();
    let text = "lorem ipsum dolor sit amet \u{5D0}\u{5D1} ".repeat(2000);
    let started = std::time::Instant::now();
    let l = library
        .layout(&RichText::plain(text.clone()), &style(), &wrapped(600.0))
        .unwrap();
    assert!(l.lines.len() > 100);
    let middle = l.lines[l.lines.len() / 2].baseline;
    let hit = l.hit_test(glam::Vec2::new(300.0, middle));
    assert!(hit > 0 && hit < text.len());
    assert!(!l.selection_rects(0..text.len()).is_empty());
    let elapsed = started.elapsed();
    println!("60 KB layout + queries: {elapsed:?}");
    assert!(elapsed.as_secs() < 20, "{elapsed:?}");
}
