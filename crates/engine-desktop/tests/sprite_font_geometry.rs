//! CPU-side sprite and text geometry that the renderer uploads verbatim:
//! quad corners and UVs, glyph placement, wrapping, fallback glyphs and
//! font-file validation. Runs without any device.

use engine_project::content::{
    FontAtlas, FontGlyph, Mesh, Texture, animation_frame, camera_viewport, load_font,
    logical_pointer, sprite_quad, text_mesh,
};
use std::collections::BTreeMap;

fn texture(width: u32, height: u32) -> Texture {
    Texture {
        width,
        height,
        pixels: vec![255; (width * height * 4) as usize],
    }
}

fn glyph(source: [u32; 4], advance: f32, bearing: [f32; 2]) -> FontGlyph {
    FontGlyph {
        source,
        advance,
        bearing,
    }
}

/// 8x8 atlas font: '?' fallback (advance 4), 'A' (advance 5, bearing 1,2), ' ' (advance 3, no quad).
fn font() -> FontAtlas {
    FontAtlas {
        texture: "atlas".into(),
        line_height: 10.0,
        fallback: '?' as u32,
        glyphs: BTreeMap::from([
            ('?' as u32, glyph([0, 0, 4, 8], 4.0, [0.0, 0.0])),
            ('A' as u32, glyph([4, 0, 4, 8], 5.0, [1.0, 2.0])),
            ('B' as u32, glyph([0, 0, 2, 2], 2.0, [0.0, 0.0])),
            (' ' as u32, glyph([0, 0, 0, 0], 3.0, [0.0, 0.0])),
        ]),
    }
}

fn code(error: &engine_project::Error) -> &str {
    &error.diagnostics[0].code
}

fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-5, "{a} != {b}");
}

fn text(s: &str, origin: [f32; 2], wrap: f32, scale: f32) -> Mesh {
    text_mesh(&font(), &texture(8, 8), s, origin, wrap, scale, [1.0; 4]).unwrap()
}

fn quad_origin(mesh: &Mesh, quad: usize) -> [f32; 2] {
    // Vertex 3 of each quad is the top-left corner after the Y flip.
    let v = &mesh.vertices[quad * 4 + 3];
    [v.position[0], v.position[1]]
}

// ---- sprite_quad -----------------------------------------------------------------------

macro_rules! pivot_cases {
    ($($name:ident: $pivot:expr => ($x:expr, $y:expr);)*) => {$(
        #[test]
        fn $name() {
            let t = texture(4, 4);
            let m = sprite_quad([10.0, 20.0, 0.0], [4.0, 2.0], $pivot, [0, 0, 4, 4], &t, [1.0; 4]).unwrap();
            // Vertex 0 is the bottom-left corner.
            assert_eq!([m.vertices[0].position[0], m.vertices[0].position[1]], [$x, $y]);
            assert_eq!([m.vertices[2].position[0], m.vertices[2].position[1]], [$x + 4.0, $y + 2.0]);
        }
    )*};
}

pivot_cases! {
    bottom_left_pivot_puts_the_corner_at_the_position: [0.0, 0.0] => (10.0, 20.0);
    centre_pivot_centres_the_quad_on_the_position: [0.5, 0.5] => (8.0, 19.0);
    top_right_pivot_hangs_the_quad_down_and_left: [1.0, 1.0] => (6.0, 18.0);
    bottom_centre_pivot_is_the_usual_character_anchor: [0.5, 0.0] => (8.0, 20.0);
}

#[test]
fn quad_has_two_counter_clockwise_triangles() {
    let m = sprite_quad(
        [0.0; 3],
        [1.0, 1.0],
        [0.0; 2],
        [0, 0, 4, 4],
        &texture(4, 4),
        [1.0; 4],
    )
    .unwrap();
    assert_eq!(m.vertices.len(), 4);
    assert_eq!(m.indices, [0, 1, 2, 0, 2, 3]);
    let p = |i: usize| m.vertices[i].position;
    assert_eq!(
        [p(0), p(1), p(2), p(3)],
        [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0]
        ]
    );
}

#[test]
fn depth_and_tint_are_copied_to_every_vertex() {
    let tint = [0.25, 0.5, 0.75, 0.125];
    let m = sprite_quad(
        [0.0, 0.0, 0.375],
        [2.0, 2.0],
        [0.0; 2],
        [0, 0, 1, 1],
        &texture(2, 2),
        tint,
    )
    .unwrap();
    for v in &m.vertices {
        assert_eq!(v.position[2], 0.375);
        assert_eq!(v.color, tint);
    }
}

#[test]
fn full_texture_rectangle_covers_the_whole_uv_square() {
    let m = sprite_quad(
        [0.0; 3],
        [1.0; 2],
        [0.0; 2],
        [0, 0, 8, 8],
        &texture(8, 8),
        [1.0; 4],
    )
    .unwrap();
    let uv: Vec<[f32; 2]> = m.vertices.iter().map(|v| v.uv).collect();
    // Bottom edge samples the bottom of the texture (v = 1); top edge samples v = 0.
    assert_eq!(uv, [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
}

#[test]
fn atlas_rectangle_maps_pixel_rows_from_the_top_left() {
    let m = sprite_quad(
        [0.0; 3],
        [1.0; 2],
        [0.0; 2],
        [1, 2, 2, 4],
        &texture(4, 8),
        [1.0; 4],
    )
    .unwrap();
    close(m.vertices[0].uv[0], 0.25);
    close(m.vertices[1].uv[0], 0.75);
    close(m.vertices[3].uv[1], 0.25);
    close(m.vertices[0].uv[1], 0.75);
}

#[test]
fn sprite_touching_the_far_atlas_edge_is_accepted() {
    let m = sprite_quad(
        [0.0; 3],
        [1.0; 2],
        [0.0; 2],
        [6, 6, 2, 2],
        &texture(8, 8),
        [1.0; 4],
    )
    .unwrap();
    assert_eq!(m.vertices[1].uv[0], 1.0);
    assert_eq!(m.vertices[3].uv[1], 0.75);
}

macro_rules! bad_rectangles {
    ($($name:ident: ($tw:expr, $th:expr), $rect:expr;)*) => {$(
        #[test]
        fn $name() {
            let t = Texture { width: $tw, height: $th, pixels: vec![255; ($tw * $th * 4) as usize] };
            let error = sprite_quad([0.0; 3], [1.0; 2], [0.0; 2], $rect, &t, [1.0; 4]).unwrap_err();
            assert_eq!(code(&error), "asset.sprite.rectangle");
        }
    )*};
}

bad_rectangles! {
    zero_width_rectangle_is_rejected: (4, 4), [0, 0, 0, 2];
    zero_height_rectangle_is_rejected: (4, 4), [0, 0, 2, 0];
    rectangle_past_the_right_edge_is_rejected: (4, 4), [3, 0, 2, 1];
    rectangle_past_the_bottom_edge_is_rejected: (4, 4), [0, 3, 1, 2];
    origin_outside_the_texture_is_rejected: (4, 4), [5, 0, 1, 1];
    overflowing_origin_plus_width_is_rejected: (4, 4), [u32::MAX, 0, 2, 1];
    overflowing_origin_plus_height_is_rejected: (4, 4), [0, u32::MAX, 1, 2];
}

#[test]
fn empty_texture_is_rejected() {
    let t = Texture {
        width: 0,
        height: 0,
        pixels: vec![],
    };
    let error = sprite_quad([0.0; 3], [1.0; 2], [0.0; 2], [0, 0, 1, 1], &t, [1.0; 4]).unwrap_err();
    assert_eq!(code(&error), "asset.sprite.rectangle");
}

// ---- text_mesh -------------------------------------------------------------------------

#[test]
fn empty_string_produces_an_empty_mesh() {
    let m = text("", [0.0; 2], 0.0, 1.0);
    assert!(m.vertices.is_empty() && m.indices.is_empty());
}

#[test]
fn single_glyph_is_placed_at_origin_plus_bearing_with_y_down() {
    let m = text("A", [10.0, 20.0], 0.0, 1.0);
    assert_eq!((m.vertices.len(), m.indices.len()), (4, 6));
    // Top-left = (10 + 1, 20 + 2); glyph is 4x8 so the bottom edge is 8 lower (larger y).
    assert_eq!(quad_origin(&m, 0), [11.0, 22.0]);
    let bottom_right = m.vertices[1].position;
    assert_eq!([bottom_right[0], bottom_right[1]], [15.0, 30.0]);
}

#[test]
fn glyph_uvs_come_from_its_atlas_cell() {
    let m = text("A", [0.0; 2], 0.0, 1.0);
    // 'A' occupies x in 4..8 of an 8x8 atlas.
    close(m.vertices[3].uv[0], 0.5);
    close(m.vertices[1].uv[0], 1.0);
    close(m.vertices[3].uv[1], 0.0);
    close(m.vertices[0].uv[1], 1.0);
}

#[test]
fn consecutive_glyphs_advance_by_the_glyph_advance() {
    let m = text("AA", [0.0; 2], 0.0, 1.0);
    assert_eq!(m.vertices.len(), 8);
    assert_eq!(quad_origin(&m, 1)[0] - quad_origin(&m, 0)[0], 5.0);
}

#[test]
fn indices_of_later_quads_are_offset_by_the_vertices_before_them() {
    let m = text("AA?", [0.0; 2], 0.0, 1.0);
    assert_eq!(m.indices.len(), 18);
    assert_eq!(&m.indices[6..12], &[4, 5, 6, 4, 6, 7]);
    assert_eq!(&m.indices[12..18], &[8, 9, 10, 8, 10, 11]);
    assert!(m.indices.iter().all(|i| (*i as usize) < m.vertices.len()));
}

#[test]
fn spaces_advance_without_emitting_geometry() {
    let m = text("A A", [0.0; 2], 0.0, 1.0);
    assert_eq!(m.vertices.len(), 8);
    // 5 (A) + 3 (space) between the two glyph origins.
    assert_eq!(quad_origin(&m, 1)[0] - quad_origin(&m, 0)[0], 8.0);
}

#[test]
fn newline_returns_to_the_origin_column_and_drops_one_line() {
    let m = text("A\nA", [10.0, 0.0], 0.0, 1.0);
    assert_eq!(quad_origin(&m, 0), [11.0, 2.0]);
    assert_eq!(quad_origin(&m, 1), [11.0, 12.0]);
}

#[test]
fn carriage_returns_are_ignored() {
    let unix = text("A\nA", [0.0; 2], 0.0, 1.0);
    let windows = text("A\r\nA", [0.0; 2], 0.0, 1.0);
    assert_eq!(unix.vertices, windows.vertices);
    assert_eq!(unix.indices, windows.indices);
}

#[test]
fn blank_lines_still_advance_the_baseline() {
    let m = text("A\n\nA", [0.0; 2], 0.0, 1.0);
    assert_eq!(quad_origin(&m, 1)[1] - quad_origin(&m, 0)[1], 20.0);
}

#[test]
fn unknown_characters_use_the_fallback_glyph() {
    let fallback = text("?", [0.0; 2], 0.0, 1.0);
    let unknown = text("\u{3a9}", [0.0; 2], 0.0, 1.0);
    assert_eq!(unknown.vertices, fallback.vertices);
    let after = text("\u{3a9}A", [0.0; 2], 0.0, 1.0);
    assert_eq!(
        quad_origin(&after, 1)[0] - quad_origin(&after, 0)[0],
        4.0 + 1.0
    );
}

#[test]
fn scale_multiplies_size_bearing_and_advance() {
    let m = text("AA", [0.0; 2], 0.0, 2.0);
    // First glyph top-left: bearing (1, 2) * 2.
    assert_eq!(quad_origin(&m, 0), [2.0, 4.0]);
    assert_eq!(quad_origin(&m, 1)[0] - quad_origin(&m, 0)[0], 10.0);
    let right = m.vertices[1].position;
    let left = m.vertices[0].position;
    assert_eq!(right[0] - left[0], 8.0, "glyph width scales");
    assert_eq!(
        left[1] - m.vertices[3].position[1],
        16.0,
        "glyph height scales"
    );
}

#[test]
fn scale_also_scales_line_height() {
    let m = text("A\nA", [0.0; 2], 0.0, 0.5);
    assert_eq!(quad_origin(&m, 1)[1] - quad_origin(&m, 0)[1], 5.0);
}

#[test]
fn colour_is_applied_to_every_glyph_vertex() {
    let color = [0.1, 0.2, 0.3, 0.4];
    let m = text_mesh(&font(), &texture(8, 8), "AA", [0.0; 2], 0.0, 1.0, color).unwrap();
    assert!(m.vertices.iter().all(|v| v.color == color));
    assert!(m.vertices.iter().all(|v| v.position[2] == 0.0));
}

#[test]
fn words_that_do_not_fit_wrap_to_the_next_line() {
    // "AA" plus its space ends at x = 13, exactly the column edge; the second word then cannot fit.
    let m = text("AA AA", [0.0; 2], 13.0, 1.0);
    assert_eq!(m.vertices.len(), 16);
    let first_line = quad_origin(&m, 0)[1];
    assert_eq!(quad_origin(&m, 1)[1], first_line);
    assert_eq!(quad_origin(&m, 2)[1], first_line + 10.0);
    assert_eq!(quad_origin(&m, 2)[0], quad_origin(&m, 0)[0]);
}

#[test]
fn words_that_fit_stay_on_the_same_line() {
    let m = text("AA AA", [0.0; 2], 100.0, 1.0);
    let y = quad_origin(&m, 0)[1];
    assert!((0..4).all(|i| quad_origin(&m, i)[1] == y));
}

#[test]
fn zero_wrap_width_never_wraps() {
    let long = "A".repeat(200);
    let m = text(&long, [0.0; 2], 0.0, 1.0);
    let y = quad_origin(&m, 0)[1];
    assert!((0..200).all(|i| quad_origin(&m, i)[1] == y));
}

#[test]
fn overlong_words_break_between_glyphs() {
    // Each 'A' is 5 wide and the column is 8 wide, so no line can hold two.
    let m = text("AAA", [0.0; 2], 8.0, 1.0);
    let ys: Vec<f32> = (0..3).map(|i| quad_origin(&m, i)[1]).collect();
    assert_eq!(ys, [2.0, 12.0, 22.0]);
}

#[test]
fn wrapping_resets_x_to_the_origin_not_to_zero() {
    let m = text("AA AA", [50.0, 0.0], 13.0, 1.0);
    assert_eq!(quad_origin(&m, 2)[0], 51.0);
}

#[test]
fn first_word_never_wraps_even_if_wider_than_the_column() {
    let m = text("AAAA", [0.0; 2], 1.0, 1.0);
    // First glyph stays on line one at x = origin; every later glyph wraps.
    assert_eq!(quad_origin(&m, 0)[1], 2.0);
    assert_eq!(quad_origin(&m, 1)[1], 12.0);
}

macro_rules! invalid_text_geometry {
    ($($name:ident: $wrap:expr, $scale:expr;)*) => {$(
        #[test]
        fn $name() {
            let error = text_mesh(&font(), &texture(8, 8), "A", [0.0; 2], $wrap, $scale, [1.0; 4]).unwrap_err();
            assert_eq!(code(&error), "ui.text.invalid");
        }
    )*};
}

invalid_text_geometry! {
    zero_scale_is_rejected: 0.0, 0.0;
    negative_scale_is_rejected: 0.0, -1.0;
    nan_scale_is_rejected: 0.0, f32::NAN;
    infinite_scale_is_rejected: 0.0, f32::INFINITY;
    negative_wrap_width_is_rejected: -1.0, 1.0;
    nan_wrap_width_is_rejected: f32::NAN, 1.0;
    infinite_wrap_width_is_rejected: f32::INFINITY, 1.0;
}

#[test]
fn text_longer_than_64_kib_is_rejected_but_the_limit_itself_is_allowed() {
    let at_limit = "A".repeat(65_536);
    assert_eq!(
        text(&at_limit, [0.0; 2], 0.0, 1.0).vertices.len(),
        65_536 * 4
    );
    let over = "A".repeat(65_537);
    let error =
        text_mesh(&font(), &texture(8, 8), &over, [0.0; 2], 0.0, 1.0, [1.0; 4]).unwrap_err();
    assert_eq!(code(&error), "ui.text.invalid");
}

#[test]
fn glyph_outside_the_atlas_texture_fails_the_whole_mesh() {
    let error = text_mesh(&font(), &texture(4, 4), "A", [0.0; 2], 0.0, 1.0, [1.0; 4]).unwrap_err();
    assert_eq!(code(&error), "asset.sprite.rectangle");
}

// ---- FontAtlas -------------------------------------------------------------------------

#[test]
fn glyph_lookup_falls_back_for_unmapped_characters() {
    let f = font();
    assert_eq!(f.glyph('A').advance, 5.0);
    assert_eq!(f.glyph('z').advance, 4.0);
    assert_eq!(f.glyph('\u{1f600}').advance, 4.0);
}

#[test]
fn atlas_validation_accepts_glyphs_inside_the_texture() {
    font().validate(8, 8).unwrap();
}

macro_rules! invalid_atlas {
    ($($name:ident: $edit:expr;)*) => {$(
        #[test]
        fn $name() {
            let mut f = font();
            let edit: fn(&mut FontAtlas) = $edit;
            edit(&mut f);
            assert_eq!(code(&f.validate(8, 8).unwrap_err()), "asset.font.invalid");
        }
    )*};
}

invalid_atlas! {
    atlas_rejects_glyph_past_the_right_edge: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().source = [6, 0, 4, 8];
    atlas_rejects_glyph_past_the_bottom_edge: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().source = [0, 4, 2, 8];
    atlas_rejects_negative_advance: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().advance = -1.0;
    atlas_rejects_nan_advance: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().advance = f32::NAN;
    atlas_rejects_infinite_bearing: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().bearing = [0.0, f32::INFINITY];
    atlas_rejects_overflowing_source_rectangle: |f| f.glyphs.get_mut(&('A' as u32)).unwrap().source = [u32::MAX, 0, 2, 1];
}

// ---- load_font -------------------------------------------------------------------------

struct TempFile(std::path::PathBuf);
impl TempFile {
    fn with(name: &str, contents: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("engine-desktop-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        Self(path)
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn font_json(schema: u32, line_height: f32, fallback: u32, glyphs: &str) -> String {
    format!(
        r#"{{"schema":{schema},"texture":"atlas","lineHeight":{line_height},"fallback":{fallback},"glyphs":[{glyphs}]}}"#
    )
}
const Q: &str = r#"{"codepoint":63,"source":[0,0,4,8],"advance":4.0,"bearing":[0.0,0.0]}"#;

#[test]
fn font_file_loads_glyphs_and_metrics() {
    let file = TempFile::with("ok.font.json", &font_json(1, 12.0, 63, Q));
    let f = load_font(&file.0).unwrap();
    assert_eq!(
        (f.texture.as_str(), f.line_height, f.fallback),
        ("atlas", 12.0, 63)
    );
    assert_eq!(f.glyphs[&63].source, [0, 0, 4, 8]);
}

macro_rules! bad_font_files {
    ($($name:ident: $json:expr;)*) => {$(
        #[test]
        fn $name() {
            let file = TempFile::with(concat!(stringify!($name), ".json"), &*$json);
            assert!(load_font(&file.0).is_err());
        }
    )*};
}

bad_font_files! {
    font_file_rejects_unknown_schema_version: font_json(2, 12.0, 63, Q);
    font_file_rejects_zero_line_height: font_json(1, 0.0, 63, Q);
    font_file_rejects_negative_line_height: font_json(1, -3.0, 63, Q);
    font_file_rejects_missing_fallback_glyph: font_json(1, 12.0, 65, Q);
    font_file_rejects_empty_glyph_list: font_json(1, 12.0, 63, "");
    font_file_rejects_duplicate_codepoints: font_json(1, 12.0, 63, &format!("{Q},{Q}"));
    font_file_rejects_surrogate_codepoints: font_json(1, 12.0, 63, &format!(r#"{Q},{{"codepoint":55296,"source":[0,0,1,1],"advance":1.0,"bearing":[0.0,0.0]}}"#));
    font_file_rejects_unknown_fields: format!(r#"{{"schema":1,"texture":"a","lineHeight":1.0,"fallback":63,"extra":1,"glyphs":[{Q}]}}"#);
    font_file_rejects_malformed_json: "{ not json";
}

// ---- animation and viewport helpers ----------------------------------------------------

macro_rules! animation_cases {
    ($($name:ident: ($elapsed:expr, $fps:expr, $count:expr, $looping:expr) => $frame:expr;)*) => {$(
        #[test]
        fn $name() {
            assert_eq!(animation_frame($elapsed, $fps, $count, $looping), $frame);
        }
    )*};
}

animation_cases! {
    animation_starts_on_frame_zero: (0.0, 10.0, 4, true) => 0;
    animation_advances_at_the_frame_rate: (0.35, 10.0, 4, true) => 3;
    looping_animation_wraps_to_the_start: (0.4, 10.0, 4, true) => 0;
    looping_animation_wraps_many_times: (2.75, 10.0, 4, true) => 3;
    one_shot_animation_holds_the_last_frame: (0.4, 10.0, 4, false) => 3;
    one_shot_animation_holds_after_a_long_time: (1000.0, 10.0, 4, false) => 3;
    negative_elapsed_time_selects_the_first_frame: (-1.0, 10.0, 4, true) => 0;
    nan_elapsed_time_selects_the_first_frame: (f64::NAN, 10.0, 4, true) => 0;
    infinite_elapsed_time_selects_the_first_frame: (f64::INFINITY, 10.0, 4, false) => 0;
    zero_frame_rate_selects_the_first_frame: (5.0, 0.0, 4, true) => 0;
    negative_frame_rate_selects_the_first_frame: (5.0, -4.0, 4, true) => 0;
    infinite_frame_rate_selects_the_first_frame: (5.0, f32::INFINITY, 4, true) => 0;
    empty_animation_selects_frame_zero: (5.0, 10.0, 0, true) => 0;
    single_frame_animation_always_shows_it: (9.0, 10.0, 1, true) => 0;
}

#[test]
fn camera_viewport_letterboxes_with_integer_scale_above_one() {
    let v = camera_viewport([320, 180], [1000, 700]);
    assert_eq!((v.scale, v.width, v.height), (3.0, 960.0, 540.0));
    assert_eq!((v.x, v.y), (20.0, 80.0));
}

#[test]
fn camera_viewport_uses_fractional_scale_below_one() {
    let v = camera_viewport([320, 180], [160, 90]);
    assert_eq!(
        (v.scale, v.width, v.height, v.x, v.y),
        (0.5, 160.0, 90.0, 0.0, 0.0)
    );
}

#[test]
fn camera_viewport_of_degenerate_sizes_is_empty() {
    for (logical, framebuffer) in [
        ([0, 180], [100, 100]),
        ([320, 0], [100, 100]),
        ([320, 180], [0, 100]),
        ([320, 180], [100, 0]),
    ] {
        let v = camera_viewport(logical, framebuffer);
        assert_eq!((v.width, v.height, v.scale), (0.0, 0.0, 0.0));
    }
}

#[test]
fn logical_pointer_accounts_for_letterbox_and_dpi() {
    // Window is 500x350 logical points, framebuffer 1000x700 (scale factor 2): pointer (260, 190)
    // lands at physical (520, 380) -> viewport origin (20, 80) at scale 3 -> (166.67, 100).
    let p = logical_pointer([260.0, 190.0], [500, 350], [1000, 700], [320, 180]);
    close(p[0], 500.0 / 3.0);
    close(p[1], 100.0);
}

#[test]
fn logical_pointer_rejects_degenerate_or_non_finite_input() {
    assert_eq!(
        logical_pointer([1.0, 1.0], [0, 100], [100, 100], [320, 180]),
        [-1.0, -1.0]
    );
    assert_eq!(
        logical_pointer([f32::NAN, 1.0], [100, 100], [100, 100], [320, 180]),
        [-1.0, -1.0]
    );
    assert_eq!(
        logical_pointer([1.0, 1.0], [100, 100], [0, 0], [320, 180]),
        [-1.0, -1.0]
    );
}
