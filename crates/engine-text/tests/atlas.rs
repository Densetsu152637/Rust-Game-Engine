mod common;

use common::*;
use engine_text::{
    AtlasConfig, AtlasGlyph, AtlasMode, FontLibrary, GlyphAtlas, LayoutOptions, PixelRect,
    Synthesis, build_quads, transform_quads,
};
use glam::{Mat4, Vec2, Vec3};

fn glyph_id(library: &FontLibrary, c: char) -> u32 {
    let l = layout(library, &c.to_string(), &LayoutOptions::default());
    l.glyphs[0].glyph_id
}

fn alpha(atlas: &GlyphAtlas, page: u32, x: u32, y: u32) -> u8 {
    let p = &atlas.pages()[page as usize];
    p.pixels()[((y * p.size() + x) * 4 + 3) as usize]
}

fn bitmap(atlas: &GlyphAtlas, g: &AtlasGlyph) -> Vec<Vec<u8>> {
    (0..g.rect.height)
        .map(|y| {
            (0..g.rect.width)
                .map(|x| alpha(atlas, g.page, g.rect.x + x, g.rect.y + y))
                .collect()
        })
        .collect()
}

#[test]
fn packing_has_no_overlaps_and_stays_in_bounds() {
    let library = library();
    let ubuntu = face(&library, UBUNTU);
    let mut atlas = GlyphAtlas::new(AtlasConfig {
        page_size: 256,
        max_pages: 16,
        ..AtlasConfig::default()
    })
    .unwrap();
    let ids: Vec<u32> = ('!'..='~').map(|c| glyph_id(&library, c)).collect();
    let mut placed: Vec<AtlasGlyph> = Vec::new();
    for size in [12.0, 24.0, 48.0] {
        for &id in &ids {
            if let Some(g) = atlas
                .glyph(&library, ubuntu, id, size, Synthesis::default())
                .unwrap()
            {
                placed.push(g);
            }
        }
    }
    assert!(placed.len() > 250);
    assert!(atlas.pages().len() > 1, "multiple pages used");
    let white = PixelRect {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };
    for (i, a) in placed.iter().enumerate() {
        assert!(a.rect.right() <= 256 && a.rect.bottom() <= 256);
        assert!(!a.rect.intersects(&white), "white block reserved");
        for b in &placed[i + 1..] {
            assert!(
                a.page != b.page || !a.rect.intersects(&b.rect),
                "{a:?} overlaps {b:?}"
            );
        }
        assert!(a.uv.u0 < a.uv.u1 && a.uv.v1 <= 1.0);
    }
    // Cached lookups return the same placement.
    let again = atlas
        .glyph(&library, ubuntu, ids[0], 12.0, Synthesis::default())
        .unwrap();
    assert_eq!(again, Some(placed[0]));
}

#[test]
fn coverage_is_inside_the_glyph_and_zero_in_the_padding() {
    let library = library();
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    let g = atlas
        .glyph(
            &library,
            face(&library, UBUNTU),
            glyph_id(&library, 'H'),
            40.0,
            Synthesis::default(),
        )
        .unwrap()
        .unwrap();
    let px = bitmap(&atlas, &g);
    let (w, h) = (g.rect.width as usize, g.rect.height as usize);
    // Padding ring (1 px) is empty.
    for (y, row) in px.iter().enumerate() {
        for (x, &a) in row.iter().enumerate() {
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                assert_eq!(a, 0, "padding at {x},{y}");
            }
        }
    }
    // Interior has solid stems and empty counters.
    let interior = px[1..h - 1].iter().flat_map(|r| r[1..w - 1].iter());
    assert!(interior.clone().filter(|&&a| a == 255).count() > 50);
    assert!(interior.filter(|&&a| a == 0).count() > 20);
    // Placement: H sits on the baseline (bottom near 0) and rises about cap height.
    assert!(g.top < -20.0 && (g.top + g.rect.height as f32).abs() <= 2.0);
    // Exactly the page's RGB channels are white.
    let page = &atlas.pages()[g.page as usize];
    assert!(page.pixels().chunks(4).all(|p| p[..3] == [255, 255, 255]));
}

#[test]
fn sdf_sign_and_smooth_gradient() {
    let library = library();
    let spread = 6.0;
    let mut atlas = GlyphAtlas::new(AtlasConfig {
        mode: AtlasMode::Sdf {
            base_px: 48.0,
            spread_px: spread,
        },
        ..AtlasConfig::default()
    })
    .unwrap();
    let g = atlas
        .glyph(
            &library,
            face(&library, UBUNTU),
            glyph_id(&library, 'l'),
            20.0,
            Synthesis::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(g.raster_px, 48.0, "SDF rasterises at the base size");
    let px = bitmap(&atlas, &g);
    let row = &px[px.len() / 2];
    let max = *row.iter().max().unwrap();
    assert!(max > 150, "stem centre is inside: {max}");
    assert_eq!(row[0], 0, "far outside is zero");
    assert_eq!(*row.last().unwrap(), 0);
    assert!(px[0].iter().all(|&a| a < 128) && px[px.len() - 1].iter().all(|&a| a < 128));
    // Distance is 1-Lipschitz: at most 255 / (2 * spread) per pixel (plus rounding).
    let step_limit = (255.0 / (2.0 * spread)).ceil() as i32 + 1;
    for pair in row.windows(2) {
        assert!(
            (pair[1] as i32 - pair[0] as i32).abs() <= step_limit,
            "{row:?}"
        );
    }
    // Rises monotonically from the left border to the stem centre, crossing the 128 edge.
    let peak = row.iter().position(|&a| a == max).unwrap();
    assert!(row[..=peak].windows(2).all(|w| w[1] >= w[0]));
    assert!(row[..peak].iter().any(|&a| a > 0 && a < 128) && row[..peak].iter().any(|&a| a >= 128));
}

#[test]
fn lru_eviction_keeps_page_count_bounded_and_spares_hot_pages() {
    let library = library();
    let ubuntu = face(&library, UBUNTU);
    let mut atlas = GlyphAtlas::new(AtlasConfig {
        page_size: 64,
        max_pages: 2,
        ..AtlasConfig::default()
    })
    .unwrap();
    let hot = glyph_id(&library, 'H');
    let ids: Vec<u32> = ('a'..='z').map(|c| glyph_id(&library, c)).collect();
    let first = atlas
        .glyph(&library, ubuntu, hot, 20.0, Synthesis::default())
        .unwrap()
        .unwrap();
    let hot_generation = atlas.pages()[first.page as usize].generation();
    for (frame, &id) in ids.iter().cycle().take(120).enumerate() {
        atlas.begin_frame();
        let size = 18.0 + (frame % 5) as f32;
        let hot_now = atlas
            .glyph(&library, ubuntu, hot, 20.0, Synthesis::default())
            .unwrap()
            .unwrap();
        assert_eq!(hot_now, first, "hot glyph never moves");
        atlas
            .glyph(&library, ubuntu, id, size, Synthesis::default())
            .unwrap();
        assert!(atlas.pages().len() <= 2);
    }
    assert!(atlas.evictions() > 0);
    assert_eq!(
        atlas.pages()[first.page as usize].generation(),
        hot_generation
    );
    let other = 1 - first.page as usize;
    assert!(
        atlas.pages()[other].generation() > 1,
        "the cold page was recycled"
    );
}

#[test]
fn full_atlas_within_one_frame_is_an_error_not_a_panic() {
    let library = library();
    let ubuntu = face(&library, UBUNTU);
    let mut atlas = GlyphAtlas::new(AtlasConfig {
        page_size: 64,
        max_pages: 1,
        ..AtlasConfig::default()
    })
    .unwrap();
    let err = ('A'..='Z')
        .map(|c| {
            atlas.glyph(
                &library,
                ubuntu,
                glyph_id(&library, c),
                30.0,
                Synthesis::default(),
            )
        })
        .find_map(Result::err)
        .expect("one frame cannot hold 26 large glyphs on one 64 px page");
    assert_eq!(err.code(), "text.atlas.full");
    let err = atlas
        .glyph(
            &library,
            ubuntu,
            glyph_id(&library, 'W'),
            200.0,
            Synthesis::default(),
        )
        .unwrap_err();
    assert_eq!(err.code(), "text.atlas.glyph_too_large");
}

#[test]
fn dirty_regions_cover_only_new_glyphs() {
    let library = library();
    let ubuntu = face(&library, UBUNTU);
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    atlas
        .glyph(
            &library,
            ubuntu,
            glyph_id(&library, 'a'),
            16.0,
            Synthesis::default(),
        )
        .unwrap();
    let first = atlas.take_dirty();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].rect.width, 1024, "a new page is uploaded whole");
    assert!(atlas.take_dirty().is_empty());
    let g = atlas
        .glyph(
            &library,
            ubuntu,
            glyph_id(&library, 'b'),
            16.0,
            Synthesis::default(),
        )
        .unwrap()
        .unwrap();
    let dirty = atlas.take_dirty();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].rect, g.rect);
    // Cache hits change nothing.
    atlas
        .glyph(
            &library,
            ubuntu,
            glyph_id(&library, 'b'),
            16.0,
            Synthesis::default(),
        )
        .unwrap();
    assert!(atlas.take_dirty().is_empty());
}

#[test]
fn quads_follow_glyphs_and_scale_in_sdf_mode() {
    let library = library();
    let l = layout(&library, "Hi there", &LayoutOptions::default());
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    let origin = Vec2::new(100.0, 50.0);
    let quads = build_quads(&l, &library, &mut atlas, origin).unwrap();
    assert_eq!(quads.len(), 7, "the space has no outline");
    for (q, g) in quads
        .iter()
        .zip(l.glyphs.iter().filter(|g| g.cluster != (2..3)))
    {
        assert!((q.rect.x - (origin.x + g.x)).abs() < 4.0);
        assert!(q.rect.y < origin.y + g.y && q.rect.bottom() > origin.y + g.y - 30.0);
        assert_eq!(
            q.rect.x,
            q.rect.x.round(),
            "coverage quads are pixel aligned"
        );
    }

    let mut sdf = GlyphAtlas::new(AtlasConfig::sdf()).unwrap();
    let mut height_at = |size: f32| {
        let l = library
            .layout(
                &engine_text::RichText::plain("H"),
                &engine_text::TextStyle::new("Ubuntu", size),
                &LayoutOptions::default(),
            )
            .unwrap();
        build_quads(&l, &library, &mut sdf, Vec2::ZERO).unwrap()[0]
            .rect
            .height
    };
    let (small, large) = (height_at(24.0), height_at(96.0));
    assert!((large / small - 4.0).abs() < 1e-3, "{small} {large}");
    assert_eq!(sdf.pages().len(), 1);
    assert_eq!(sdf.cached_glyphs(), 1, "one SDF bitmap serves every size");
}

#[test]
fn underline_quads_sample_the_white_block() {
    let library = library();
    let rich = engine_text::parse_markup("[u]under[/u]", &Default::default()).unwrap();
    let l = library
        .layout(&rich, &style(), &LayoutOptions::default())
        .unwrap();
    let mut atlas = GlyphAtlas::new(AtlasConfig::default()).unwrap();
    let quads = build_quads(&l, &library, &mut atlas, Vec2::ZERO).unwrap();
    let underline = quads.last().unwrap();
    assert_eq!(underline.rect, l.decorations[0].rect);
    let p = &atlas.pages()[underline.page as usize];
    let (x, y) = (
        (underline.uv.u0 * p.size() as f32) as u32,
        (underline.uv.v0 * p.size() as f32) as u32,
    );
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        assert_eq!(alpha(&atlas, underline.page, x - dx, y - dy), 255);
    }
}

#[test]
fn transform_quads_applies_world_matrix() {
    let library = library();
    let l = layout(&library, "A", &LayoutOptions::default());
    let mut atlas = GlyphAtlas::new(AtlasConfig::sdf()).unwrap();
    let quads = build_quads(&l, &library, &mut atlas, Vec2::ZERO).unwrap();
    let m = Mat4::from_translation(Vec3::new(5.0, 0.0, -2.0))
        * Mat4::from_scale(Vec3::new(0.01, -0.01, 1.0));
    let world = transform_quads(&quads, m);
    let r = quads[0].rect;
    assert!(world[0].corners[0].abs_diff_eq(Vec3::new(5.0 + r.x * 0.01, -r.y * 0.01, -2.0), 1e-5));
    assert!(world[0].corners[2].abs_diff_eq(
        Vec3::new(5.0 + r.right() * 0.01, -r.bottom() * 0.01, -2.0),
        1e-5
    ));
    assert_eq!(world[0].uv, quads[0].uv);
}

#[test]
fn invalid_atlas_config_is_rejected() {
    for config in [
        AtlasConfig {
            page_size: 16,
            ..AtlasConfig::default()
        },
        AtlasConfig {
            max_pages: 0,
            ..AtlasConfig::default()
        },
        AtlasConfig {
            mode: AtlasMode::Sdf {
                base_px: 32.0,
                spread_px: f32::NAN,
            },
            ..AtlasConfig::default()
        },
    ] {
        assert_eq!(
            GlyphAtlas::new(config).unwrap_err().code(),
            "text.atlas.config"
        );
    }
}

/// Optional visual check: `ENGINE_TEXT_ATLAS_PNG=<path>` writes coverage and SDF pages.
#[test]
fn write_atlas_png_when_requested() {
    let Ok(path) = std::env::var("ENGINE_TEXT_ATLAS_PNG") else {
        println!("skipped: set ENGINE_TEXT_ATLAS_PNG to write atlas pages");
        return;
    };
    let library = library();
    let text = "Hello, atlas! AV fi \u{1F600} The quick brown fox 0123456789";
    for (suffix, config) in [
        ("coverage", AtlasConfig::default()),
        ("sdf", AtlasConfig::sdf()),
    ] {
        let mut atlas = GlyphAtlas::new(AtlasConfig {
            page_size: 512,
            ..config
        })
        .unwrap();
        for size in [14.0, 28.0, 56.0] {
            let l = library
                .layout(
                    &engine_text::RichText::plain(text),
                    &engine_text::TextStyle::new("Ubuntu", size),
                    &LayoutOptions::default(),
                )
                .unwrap();
            build_quads(&l, &library, &mut atlas, Vec2::ZERO).unwrap();
        }
        let page = &atlas.pages()[0];
        // Visualise alpha on black.
        let gray: Vec<u8> = page.pixels().chunks(4).map(|p| p[3]).collect();
        let file = std::fs::File::create(format!("{path}-{suffix}.png")).unwrap();
        let mut encoder =
            png::Encoder::new(std::io::BufWriter::new(file), page.size(), page.size());
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&gray)
            .unwrap();
    }
}
