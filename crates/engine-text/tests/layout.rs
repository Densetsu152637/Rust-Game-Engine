mod common;

use common::*;
use engine_text::{
    BaseDirection, CaretMove, FallbackConfig, FontLibrary, LayoutOptions, LineHeight, Overflow,
    RichText, StyleChange, TextAlign,
};
use glam::Vec2;

fn width(library: &FontLibrary, text: &str) -> f32 {
    layout(library, text, &LayoutOptions::default()).lines[0].width
}

#[test]
fn kerning_pair_is_narrower_than_separate_glyphs() {
    let library = library();
    let pair = width(&library, "AV");
    let separate = width(&library, "A") + width(&library, "V");
    assert!(
        pair < separate - 0.5,
        "AV {pair} should kern below A+V {separate}"
    );
}

#[test]
fn ligature_forms_one_cluster_with_interior_caret_stop() {
    let library = library();
    let l = layout(&library, "fi", &LayoutOptions::default());
    assert_eq!(l.glyphs.len(), 1, "Ubuntu has an fi ligature");
    assert_eq!(l.glyphs[0].cluster, 0..2);
    // The caret between f and i splits the ligature advance evenly.
    let mid = l.caret_rect(1).x;
    assert!((mid - l.lines[0].width / 2.0).abs() < 0.01, "mid {mid}");
    assert_eq!(l.hit_test(Vec2::new(mid, 10.0)), 1);
}

#[test]
fn combining_mark_stays_in_one_grapheme_cluster() {
    let library = library();
    let l = layout(&library, "e\u{301}x", &LayoutOptions::default());
    let first: Vec<_> = l.glyphs.iter().filter(|g| g.cluster.start == 0).collect();
    assert!(!first.is_empty());
    assert!(first.iter().all(|g| g.cluster == (0..3)));
    assert_eq!(l.move_caret(0, CaretMove::NextGrapheme), 3);
    assert_eq!(l.move_caret(3, CaretMove::PrevGrapheme), 0);
    assert_eq!(l.snap_index(1), 0, "inside a grapheme snaps down");
}

#[test]
fn fallback_picks_emoji_face_for_emoji() {
    let library = library();
    let l = layout(&library, "a\u{1F600}b", &LayoutOptions::default());
    let emoji = l.glyphs.iter().find(|g| g.cluster.start == 1).unwrap();
    assert_eq!(emoji.face, face(&library, EMOJI));
    assert_ne!(emoji.glyph_id, 0, "not .notdef");
    for g in l.glyphs.iter().filter(|g| g.cluster.start != 1) {
        assert_eq!(g.face, face(&library, UBUNTU));
    }
}

/// A character Ubuntu and Noto Emoji lack but Hack has, found from a candidate list.
fn hack_only_char(library: &FontLibrary) -> char {
    let candidates = [
        '\u{2500}', '\u{2588}', '\u{2591}', '\u{25A0}', '\u{2318}', '\u{2261}', '\u{2502}',
        '\u{256C}',
    ];
    candidates
        .into_iter()
        .find(|&c| {
            !library.face_covers(face(library, UBUNTU), c)
                && !library.face_covers(face(library, EMOJI), c)
                && library.face_covers(face(library, HACK), c)
        })
        .expect("a Hack-only test character")
}

#[test]
fn language_and_script_overrides_choose_fallback_faces() {
    let mut library = library();
    let c = hack_only_char(&library);
    let text = format!("a{c}");
    let at = 1;
    let face_of = |library: &FontLibrary, options: &LayoutOptions| {
        let l = library
            .layout(&RichText::plain(text.clone()), &style(), options)
            .unwrap();
        l.glyphs
            .iter()
            .find(|g| g.cluster.start == at)
            .unwrap()
            .face
    };
    // Without overrides nothing covers it: the primary face renders .notdef.
    assert_eq!(
        face_of(&library, &LayoutOptions::default()),
        face(&library, UBUNTU)
    );

    library
        .set_fallback(
            FallbackConfig::new()
                .with_default(["Noto Emoji"])
                .with_language("x-Test", ["Hack"]),
        )
        .unwrap();
    let with_language = LayoutOptions {
        language: Some("x-test-variant".into()),
        ..LayoutOptions::default()
    };
    assert_eq!(face_of(&library, &with_language), face(&library, HACK));
    assert_eq!(
        face_of(&library, &LayoutOptions::default()),
        face(&library, UBUNTU)
    );

    // The box-drawing character is script Common and resolves to its Latin neighbour.
    library
        .set_fallback(FallbackConfig::new().with_script("latn", ["Hack"]))
        .unwrap();
    assert_eq!(
        face_of(&library, &LayoutOptions::default()),
        face(&library, HACK)
    );
}

#[test]
fn fallback_chain_orders_language_prefixes_script_then_default() {
    let config = FallbackConfig::new()
        .with_default(["D"])
        .with_script("Hani", ["S"])
        .with_language("zh", ["L1"])
        .with_language("zh-Hant", ["L2"]);
    let chain: Vec<&str> = config.chain(Some("zh-Hant-TW"), "Hani").collect();
    assert_eq!(chain, ["L2", "L1", "S", "D"]);
}

#[test]
fn wrapping_matches_greedy_word_count_and_never_exceeds_width() {
    let library = library();
    let text = "The quick brown fox jumps over the lazy dog again and again";
    let max = 230.0;
    let l = layout(&library, text, &wrapped(max));
    // Independent greedy fill over measured word widths.
    let space = width(&library, "a a") - width(&library, "aa");
    let mut expected = 1;
    let mut line = 0.0;
    for word in text.split(' ') {
        let w = width(&library, word);
        if line > 0.0 && line + space + w > max {
            expected += 1;
            line = w;
        } else if line > 0.0 {
            line += space + w;
        } else {
            line = w;
        }
    }
    assert!(expected > 2);
    assert_eq!(l.lines.len(), expected);
    for line in &l.lines {
        assert!(line.width <= max + 1e-3, "line {line:?}");
        assert!(!line.hard_break);
    }
    // Lines cover the text contiguously.
    for pair in l.lines.windows(2) {
        assert_eq!(pair[0].range.end, pair[1].range.start);
    }
    assert_eq!(l.lines.last().unwrap().range.end, text.len());
}

#[test]
fn long_word_breaks_at_grapheme_boundaries() {
    let library = library();
    let l = layout(&library, "abcdefghijklmnopqrstuvwxyz", &wrapped(80.0));
    assert!(l.lines.len() >= 4);
    assert!(l.lines.iter().all(|line| line.width <= 80.0 + 1e-3));
}

#[test]
fn explicit_newlines_make_hard_lines_and_trailing_empty_line() {
    let library = library();
    let l = layout(&library, "ab\r\ncd\n", &LayoutOptions::default());
    assert_eq!(l.lines.len(), 3);
    assert!(l.lines[0].hard_break && l.lines[1].hard_break);
    assert_eq!(l.lines[0].range, 0..4);
    assert_eq!(l.lines[2].range, 7..7);
    assert!(l.lines[2].top > l.lines[1].top);
    // The caret after the final newline sits on the empty last line.
    assert_eq!(l.line_of(7), 2);
    assert_eq!(l.move_caret(1, CaretMove::LineEnd), 2);
}

#[test]
fn bidi_mixed_line_reorders_hebrew_visually() {
    let library = library();
    // a b c ' ' alef bet gimel ' ' d e f
    let text = "abc \u{5D0}\u{5D1}\u{5D2} def";
    let l = layout(&library, text, &LayoutOptions::default());
    assert!(!l.lines[0].rtl);
    let (alef, bet, gimel) = (glyph_x_at(&l, 4), glyph_x_at(&l, 6), glyph_x_at(&l, 8));
    assert!(alef > bet && bet > gimel, "{alef} {bet} {gimel}");
    let (c, d) = (glyph_x_at(&l, 2), glyph_x_at(&l, 11));
    assert!(c < gimel && alef < d, "hebrew stays between c and d");
}

#[test]
fn rtl_paragraph_aligns_start_to_the_right() {
    let library = library();
    let text = "\u{5D0}\u{5D1} abc";
    let l = layout(&library, text, &wrapped(400.0));
    let line = &l.lines[0];
    assert!(line.rtl, "first strong character is Hebrew");
    assert!((line.left + line.width - 400.0).abs() < 0.01);
    // Latin run sits left of the Hebrew run in an RTL paragraph.
    assert!(glyph_x_at(&l, 5) < glyph_x_at(&l, 0));

    let forced = library
        .layout(
            &RichText::plain("abc"),
            &style(),
            &LayoutOptions {
                direction: BaseDirection::Rtl,
                max_width: Some(400.0),
                ..LayoutOptions::default()
            },
        )
        .unwrap();
    assert!(forced.lines[0].rtl);
    assert!((forced.lines[0].left + forced.lines[0].width - 400.0).abs() < 0.01);
}

#[test]
fn alignment_and_line_height() {
    let library = library();
    let base = LayoutOptions {
        max_width: Some(500.0),
        line_height: LineHeight::Absolute(40.0),
        ..LayoutOptions::default()
    };
    let center = layout(
        &library,
        "ab\ncd",
        &LayoutOptions {
            align: TextAlign::Center,
            ..base.clone()
        },
    );
    let line = &center.lines[0];
    assert!((line.left - (500.0 - line.width) / 2.0).abs() < 0.01);
    assert_eq!(center.lines[1].top, 40.0);
    let right = layout(
        &library,
        "ab",
        &LayoutOptions {
            align: TextAlign::Right,
            ..base.clone()
        },
    );
    assert!((right.lines[0].left + right.lines[0].width - 500.0).abs() < 0.01);
    let relative = layout(
        &library,
        "a\nb",
        &LayoutOptions {
            line_height: LineHeight::Relative(1.5),
            ..LayoutOptions::default()
        },
    );
    assert!((relative.lines[0].height - 48.0).abs() < 0.01);
}

#[test]
fn ellipsis_truncates_at_max_lines() {
    let library = library();
    let text = "one two three four five six seven eight nine ten eleven twelve";
    let options = LayoutOptions {
        max_width: Some(150.0),
        max_lines: Some(2),
        overflow: Overflow::Ellipsis,
        ..LayoutOptions::default()
    };
    let l = layout(&library, text, &options);
    assert_eq!(l.lines.len(), 2);
    assert!(l.truncated);
    let last = &l.lines[1];
    assert!(last.ellipsized && !l.lines[0].ellipsized);
    assert!(last.width <= 150.0 + 1e-3);
    let ellipsis: Vec<_> = l.glyphs.iter().filter(|g| g.cluster.is_empty()).collect();
    assert_eq!(ellipsis.len(), 1, "one U+2026 glyph");
    let rightmost = l
        .glyphs
        .iter()
        .filter(|g| g.line == 1)
        .map(|g| g.x)
        .fold(f32::MIN, f32::max);
    assert_eq!(ellipsis[0].x, rightmost);
    assert!(l.laid_out_end() < text.len());
    assert_eq!(l.move_caret(0, CaretMove::TextEnd), l.laid_out_end());

    let clipped = layout(
        &library,
        text,
        &LayoutOptions {
            overflow: Overflow::Clip,
            ..options
        },
    );
    assert_eq!(clipped.lines.len(), 2);
    assert!(clipped.glyphs.iter().all(|g| !g.cluster.is_empty()));
}

#[test]
fn caret_rect_and_hit_test_round_trip() {
    let library = library();
    for text in [
        "Hello brave new world of text",
        "\u{5D0}\u{5D1}\u{5D2} \u{5D3}\u{5D4}",
    ] {
        let l = layout(&library, text, &wrapped(140.0));
        let mut boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        boundaries.push(text.len());
        for index in boundaries {
            let rect = l.caret_rect(index);
            assert_eq!(rect.height, l.lines[l.line_of(index)].height);
            let back = l.hit_test(rect.center());
            assert_eq!(back, index, "{text:?} index {index} rect {rect:?}");
        }
    }
}

#[test]
fn rtl_caret_moves_left_as_index_grows() {
    let library = library();
    let l = layout(&library, "\u{5D0}\u{5D1}\u{5D2}", &LayoutOptions::default());
    let xs: Vec<f32> = [0, 2, 4, 6].iter().map(|&i| l.caret_rect(i).x).collect();
    assert!(xs.windows(2).all(|w| w[0] > w[1]), "{xs:?}");
    // Visual movement is opposite to logical movement in RTL text.
    assert_eq!(l.move_caret(2, CaretMove::VisualRight), 0);
    assert_eq!(l.move_caret(2, CaretMove::VisualLeft), 4);
    assert_eq!(l.move_caret(2, CaretMove::NextGrapheme), 4);
}

#[test]
fn selection_rects_per_line_and_per_bidi_piece() {
    let library = library();
    let text = "Hello brave new world of text";
    let l = layout(&library, text, &wrapped(140.0));
    assert!(l.lines.len() >= 2);
    assert_eq!(l.selection_rects(0..text.len()).len(), l.lines.len());
    assert_eq!(l.selection_rects(1..4).len(), 1);
    assert!(l.selection_rects(3..3).is_empty());

    let mixed = "abc \u{5D0}\u{5D1}\u{5D2} def";
    let m = layout(&library, mixed, &LayoutOptions::default());
    // From 'b' to after alef: Latin piece plus the alef piece at the far side of the run.
    let rects = m.selection_rects(1..6);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert!(rects.iter().all(|r| r.width > 0.0));
}

#[test]
fn word_line_and_vertical_caret_movement() {
    let library = library();
    let text = "hello  world\nsecond line";
    let l = layout(&library, text, &LayoutOptions::default());
    assert_eq!(l.move_caret(0, CaretMove::NextWord), 7);
    assert_eq!(l.move_caret(12, CaretMove::PrevWord), 7);
    assert_eq!(l.move_caret(9, CaretMove::LineStart), 0);
    assert_eq!(l.move_caret(3, CaretMove::LineEnd), 12);
    assert_eq!(l.move_caret(0, CaretMove::LineDown { x: None }), 13);
    assert_eq!(
        l.move_caret(15, CaretMove::LineUp { x: None }),
        l.hit_test(Vec2::new(l.caret_rect(15).x, 1.0))
    );
    assert_eq!(
        l.move_caret(13, CaretMove::VisualLeft),
        12,
        "wraps to previous line end"
    );
    assert_eq!(l.move_caret(0, CaretMove::TextEnd), text.len());
}

#[test]
fn rich_spans_style_glyphs_and_report_regions() {
    let library = library();
    let mut b = RichText::builder();
    b.text("plain ");
    b.push_style(StyleChange::Bold).text("bold").pop();
    b.text(" ");
    b.push_style(StyleChange::Italic).text("it").pop();
    b.text(" ");
    b.push_style(StyleChange::Underline)
        .push_style(StyleChange::Link("home".into()))
        .text("link");
    b.pop();
    b.pop();
    b.image("icon", 20.0, 10.0);
    let rich = b.build();
    let l = library
        .layout(&rich, &style(), &LayoutOptions::default())
        .unwrap();
    let bold = l.glyphs.iter().find(|g| g.cluster.start == 6).unwrap();
    assert!(bold.synthesis.bold, "Ubuntu Light has no bold face");
    let italic = l.glyphs.iter().find(|g| g.cluster.start == 11).unwrap();
    assert!(italic.synthesis.oblique && !italic.synthesis.bold);
    let plain = l.glyphs.iter().find(|g| g.cluster.start == 0).unwrap();
    assert_eq!(plain.synthesis, Default::default());
    assert_eq!(plain.span, None);

    assert_eq!(l.links.len(), 1);
    let link = &l.links[0];
    assert_eq!(link.id, "home");
    assert_eq!(&rich.text[link.range.clone()], "link");
    assert!(
        link.rect
            .contains(Vec2::new(link.rect.x + 1.0, link.rect.y + 1.0))
    );
    assert_eq!(l.decorations.len(), 1);
    let underline = &l.decorations[0];
    assert!(
        underline.rect.y > l.lines[0].baseline,
        "underline below the baseline"
    );
    assert!((underline.rect.width - link.rect.width).abs() < 0.01);

    assert_eq!(l.images.len(), 1);
    let image = &l.images[0];
    assert_eq!(image.key, "icon");
    assert_eq!((image.rect.width, image.rect.height), (20.0, 10.0));
    assert!((image.rect.bottom() - l.lines[0].baseline).abs() < 0.01);
    assert!(image.rect.x >= link.rect.right() - 0.01);
}

#[test]
fn bold_synthesis_widens_advances() {
    let library = library();
    let mut b = RichText::builder();
    b.push_style(StyleChange::Bold).text("mmmm");
    let bold = library
        .layout(&b.build(), &style(), &LayoutOptions::default())
        .unwrap();
    let regular = layout(&library, "mmmm", &LayoutOptions::default());
    let extra = bold.lines[0].width - regular.lines[0].width;
    assert!((extra - 4.0 * 32.0 / 24.0).abs() < 0.05, "extra {extra}");
}

#[test]
fn empty_text_has_one_caret_line() {
    let library = library();
    let l = layout(&library, "", &LayoutOptions::default());
    assert_eq!(l.lines.len(), 1);
    assert!(l.lines[0].height > 0.0);
    assert_eq!(l.hit_test(Vec2::new(50.0, 50.0)), 0);
    assert_eq!(l.caret_rect(0).x, 0.0);
    assert!(l.selection_rects(0..10).is_empty());
}

#[test]
fn tabs_take_four_spaces() {
    let library = library();
    let tab = width(&library, "a\tb");
    let spaces = width(&library, "a    b");
    assert!((tab - spaces).abs() < 0.05, "{tab} vs {spaces}");
}
