//! CPU-side content: mesh/texture/font loaders, sprite and text geometry,
//! animation frame selection, letterboxing and pointer mapping.
use super::common::*;
use engine_project::content::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

fn mesh_from(bytes: impl AsRef<[u8]>) -> engine_project::Result<Mesh> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.mesh");
    fs::write(&path, bytes).unwrap();
    load_mesh(&path)
}

fn texture_from(bytes: impl AsRef<[u8]>) -> engine_project::Result<Texture> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.bin");
    fs::write(&path, bytes).unwrap();
    load_texture(&path)
}

fn font_from(value: &Value) -> engine_project::Result<FontAtlas> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.json");
    fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    load_font(&path)
}

const TRI: &str = "vertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n";

fn bad_mesh(text: &str) {
    assert_eq!(code(mesh_from(text)), "asset.mesh.invalid", "{text:?}");
}

table! { bad_mesh;
    mesh_empty_file_is_invalid => ("");
    mesh_header_without_version_is_invalid => ("CGMESH");
    mesh_future_version_is_invalid => ("CGMESH 2\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_lowercase_magic_is_invalid => ("cgmesh 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_decimal_version_is_invalid => ("CGMESH 1.0\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_header_only_has_no_triangles => ("CGMESH 1\n");
    mesh_comment_only_body_has_no_triangles => ("CGMESH 1\n# vertex 0 0 0 0 0\n");
    mesh_unknown_record_is_invalid => ("CGMESH 1\nface 0 1 2\n");
    mesh_vertex_with_four_numbers_is_invalid => ("CGMESH 1\nvertex 0 0 0 0\n");
    mesh_vertex_with_non_numeric_field_is_invalid => ("CGMESH 1\nvertex a 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_nan_coordinate_is_invalid => ("CGMESH 1\nvertex NaN 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_infinite_coordinate_is_invalid => ("CGMESH 1\nvertex inf 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_overflowing_literal_is_invalid => ("CGMESH 1\nvertex 1e999 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n");
    mesh_one_vertex_is_not_a_triangle => ("CGMESH 1\nvertex 0 0 0 0 0\n");
    mesh_two_vertices_are_not_a_triangle => ("CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\n");
    mesh_four_vertices_are_not_whole_triangles => ("CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\nvertex 1 1 0 1 1\n");
    mesh_trailing_token_after_vertices_is_invalid => ("CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\nextra\n");
}

#[test]
fn mesh_vertices_preserve_position_uv_and_default_white() {
    let mesh = mesh_from(format!("CGMESH 1\n{TRI}")).unwrap();
    assert_eq!(mesh.vertices[1].position, [1.0, 0.0, 0.0]);
    assert_eq!(mesh.vertices[1].uv, [1.0, 0.0]);
    assert_eq!(mesh.vertices[2].uv, [0.0, 1.0]);
    assert_eq!(mesh.vertices[0].color, [1.0; 4]);
    assert_eq!(mesh.indices, vec![0, 1, 2]);
}

#[test]
fn mesh_with_two_triangles_gets_sequential_indices() {
    let mesh = mesh_from(format!("CGMESH 1\n{TRI}{TRI}")).unwrap();
    assert_eq!(mesh.vertices.len(), 6);
    assert_eq!(mesh.indices, (0..6).collect::<Vec<u32>>());
}

#[test]
fn mesh_comments_and_blank_lines_are_ignored() {
    let text = "CGMESH 1 # header\n\n# full line\nvertex 0 0 0 0 0 # tail\nvertex 1 0 0 1 0\n\nvertex 0 1 0 0 1\n";
    assert_eq!(mesh_from(text).unwrap().vertices.len(), 3);
}

#[test]
fn mesh_tokens_may_share_one_line() {
    let text = "CGMESH 1 vertex 0 0 0 0 0 vertex 1 0 0 1 0 vertex 0 1 0 0 1";
    assert_eq!(mesh_from(text).unwrap().vertices.len(), 3);
}

#[test]
fn mesh_accepts_windows_line_endings() {
    let text = format!("CGMESH 1\r\n{}", TRI.replace('\n', "\r\n"));
    assert_eq!(mesh_from(text).unwrap().vertices.len(), 3);
}

#[test]
fn mesh_accepts_scientific_and_negative_numbers() {
    let text = "CGMESH 1\nvertex -1.5 2e2 0.25 -0 1e-1\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n";
    let mesh = mesh_from(text).unwrap();
    assert_eq!(mesh.vertices[0].position, [-1.5, 200.0, 0.25]);
    assert!((mesh.vertices[0].uv[1] - 0.1).abs() < 1e-6);
}

#[test]
fn mesh_rejects_invalid_utf8() {
    assert_eq!(code(mesh_from([0xffu8, 0xfe, 0x00])), "asset.mesh.invalid");
}

#[test]
fn mesh_missing_file_reports_file_io() {
    let dir = tempfile::tempdir().unwrap();
    expect_code(load_mesh(&dir.path().join("none.mesh")), "project.file.io");
}

#[test]
fn mesh_of_one_hundred_triangles_loads_in_order() {
    let mut text = String::from("CGMESH 1\n");
    for i in 0..300 {
        text.push_str(&format!("vertex {i} 0 0 0 0\n"));
    }
    let mesh = mesh_from(text).unwrap();
    assert_eq!(mesh.vertices.len(), 300);
    assert_eq!(mesh.vertices[299].position[0], 299.0);
}

// ---- textures ----------------------------------------------------------------

fn bad_texture(text: &str) {
    assert_eq!(
        code(texture_from(text)),
        "asset.texture.invalid",
        "{text:?}"
    );
}

table! { bad_texture;
    texture_empty_file_is_invalid => ("");
    texture_binary_ppm_magic_is_unsupported => ("P6\n1 1\n255\nxyz");
    texture_magic_p1_is_unsupported => ("P1\n1 1\n0\n");
    texture_without_dimensions_is_invalid => ("P3\n");
    texture_zero_width_is_invalid => ("P3\n0 1\n255\n");
    texture_zero_height_is_invalid => ("P3\n1 0\n255\n");
    texture_width_above_8192_is_invalid => ("P3\n8193 1\n255\n");
    texture_height_above_8192_is_invalid => ("P3\n1 8193\n255\n");
    texture_maximum_65535_is_invalid => ("P3\n1 1\n65535\n0 0 0\n");
    texture_maximum_254_is_invalid => ("P3\n1 1\n254\n0 0 0\n");
    texture_missing_all_pixels_is_invalid => ("P3\n1 1\n255\n");
    texture_truncated_pixel_is_invalid => ("P3\n1 1\n255\n1 2\n");
    texture_second_pixel_missing_is_invalid => ("P3\n2 1\n255\n1 2 3\n");
    texture_channel_above_255_is_invalid => ("P3\n1 1\n255\n1 256 3\n");
    texture_negative_channel_is_invalid => ("P3\n1 1\n255\n1 -2 3\n");
    texture_fractional_channel_is_invalid => ("P3\n1 1\n255\n1 2.5 3\n");
    texture_non_numeric_width_is_invalid => ("P3\nwide 1\n255\n0 0 0\n");
    texture_trailing_number_is_invalid => ("P3\n1 1\n255\n1 2 3 4\n");
    texture_trailing_word_is_invalid => ("P3\n1 1\n255\n1 2 3 extra\n");
}

#[test]
fn texture_png_signature_with_garbage_body_is_invalid() {
    assert_eq!(
        code(texture_from(b"\x89PNG\r\n\x1a\ngarbage")),
        "asset.texture.invalid"
    );
}

#[test]
fn texture_ppm_pixels_are_row_major_with_opaque_alpha() {
    let t = texture_from("P3\n2 2\n255\n1 2 3  4 5 6\n7 8 9  10 11 12\n").unwrap();
    assert_eq!((t.width, t.height), (2, 2));
    assert_eq!(
        t.pixels,
        vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
    );
}

#[test]
fn texture_ppm_comments_and_arbitrary_whitespace_are_accepted() {
    let t = texture_from("P3 # magic\n# size\n1\n1 255 # max\n\t9\n8 # b\n7").unwrap();
    assert_eq!(t.pixels, vec![9, 8, 7, 255]);
}

#[test]
fn texture_ppm_extremes_of_each_channel_are_preserved() {
    let t = texture_from("P3 1 1 255 0 255 128").unwrap();
    assert_eq!(t.pixels, vec![0, 255, 128, 255]);
}

#[test]
fn texture_ppm_wide_strip_of_8192_pixels_loads() {
    let mut text = String::from("P3\n8192 1\n255\n");
    text.push_str(&"1 2 3 ".repeat(8192));
    let t = texture_from(text).unwrap();
    assert_eq!((t.width, t.height, t.pixels.len()), (8192, 1, 8192 * 4));
}

#[test]
fn texture_missing_file_reports_file_io() {
    let dir = tempfile::tempdir().unwrap();
    expect_code(
        load_texture(&dir.path().join("none.png")),
        "project.file.io",
    );
}

#[test]
fn texture_non_utf8_non_png_bytes_are_invalid() {
    assert_eq!(
        code(texture_from([0xffu8, 0x00, 0x12])),
        "asset.texture.invalid"
    );
}

fn save_png(image: impl FnOnce(&std::path::Path)) -> Texture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.png");
    image(&path);
    load_texture(&path).unwrap()
}

#[test]
fn texture_png_rgba_pixels_keep_order_and_alpha() {
    let t = save_png(|p| {
        image::RgbaImage::from_raw(2, 1, vec![10, 20, 30, 40, 50, 60, 70, 80])
            .unwrap()
            .save(p)
            .unwrap()
    });
    assert_eq!((t.width, t.height), (2, 1));
    assert_eq!(t.pixels, vec![10, 20, 30, 40, 50, 60, 70, 80]);
}

#[test]
fn texture_png_rgb_gains_opaque_alpha() {
    let t = save_png(|p| {
        image::RgbImage::from_raw(1, 1, vec![9, 8, 7])
            .unwrap()
            .save(p)
            .unwrap()
    });
    assert_eq!(t.pixels, vec![9, 8, 7, 255]);
}

#[test]
fn texture_png_grayscale_expands_to_rgb() {
    let t = save_png(|p| {
        image::GrayImage::from_raw(2, 1, vec![0, 200])
            .unwrap()
            .save(p)
            .unwrap()
    });
    assert_eq!(t.pixels, vec![0, 0, 0, 255, 200, 200, 200, 255]);
}

#[test]
fn texture_png_rows_are_top_to_bottom() {
    let t = save_png(|p| {
        image::RgbaImage::from_raw(1, 2, vec![1, 1, 1, 255, 2, 2, 2, 255])
            .unwrap()
            .save(p)
            .unwrap()
    });
    assert_eq!(&t.pixels[..4], &[1, 1, 1, 255]);
    assert_eq!(&t.pixels[4..], &[2, 2, 2, 255]);
}

#[test]
fn texture_png_wider_than_8192_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wide.png");
    image::RgbaImage::new(8193, 1).save(&path).unwrap();
    expect_code(load_texture(&path), "asset.texture.invalid");
}

#[test]
fn texture_png_taller_than_8192_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tall.png");
    image::RgbaImage::new(1, 8193).save(&path).unwrap();
    expect_code(load_texture(&path), "asset.texture.invalid");
}

#[test]
fn texture_truncated_png_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.png");
    image::RgbaImage::new(4, 4).save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        code(texture_from(&bytes[..bytes.len() / 2])),
        "asset.texture.invalid"
    );
}

// ---- fonts -------------------------------------------------------------------

fn bad_font(mutate: impl FnOnce(&mut Value), expected: &str) {
    let mut f = font_json();
    mutate(&mut f);
    expect_code(font_from(&f), expected);
}

table! { bad_font;
    font_schema_two_is_invalid => (|f| f["schema"] = json!(2), "asset.font.invalid");
    font_without_glyphs_is_invalid => (|f| f["glyphs"] = json!([]), "asset.font.invalid");
    font_empty_texture_name_is_invalid => (|f| f["texture"] = json!(""), "asset.font.invalid");
    font_zero_line_height_is_invalid => (|f| f["lineHeight"] = json!(0), "asset.font.invalid");
    font_negative_line_height_is_invalid => (|f| f["lineHeight"] = json!(-3), "asset.font.invalid");
    font_unknown_field_is_malformed => (|f| f["kerning"] = json!([]), "project.document.malformed");
    font_snake_case_line_height_is_malformed => (|f| { let v = f["lineHeight"].take(); f.as_object_mut().unwrap().remove("lineHeight"); f["line_height"] = v; }, "project.document.malformed");
    font_missing_fallback_field_is_malformed => (|f| { f.as_object_mut().unwrap().remove("fallback"); }, "project.document.malformed");
    font_duplicate_codepoint_is_invalid => (|f| f["glyphs"][1]["codepoint"] = json!(98), "asset.font.invalid");
    font_surrogate_codepoint_is_invalid => (|f| f["glyphs"][1]["codepoint"] = json!(0xD800), "asset.font.invalid");
    font_codepoint_beyond_unicode_is_invalid => (|f| f["glyphs"][1]["codepoint"] = json!(0x110000), "asset.font.invalid");
    font_fallback_must_exist_in_glyphs => (|f| f["fallback"] = json!(64), "asset.font.invalid");
    font_glyph_unknown_field_is_malformed => (|f| f["glyphs"][0]["rotation"] = json!(1), "project.document.malformed");
    font_glyph_missing_advance_is_malformed => (|f| { f["glyphs"][0].as_object_mut().unwrap().remove("advance"); }, "project.document.malformed");
    font_negative_codepoint_is_malformed => (|f| f["glyphs"][0]["codepoint"] = json!(-1), "project.document.malformed");
    font_source_rect_wrong_arity_is_malformed => (|f| f["glyphs"][0]["source"] = json!([0, 0, 1]), "project.document.malformed");
}

#[test]
fn font_file_that_is_not_json_is_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.json");
    fs::write(&path, "glyphs").unwrap();
    expect_code(load_font(&path), "project.document.malformed");
}

#[test]
fn font_loads_metrics_and_glyph_table() {
    let font = font_from(&font_json()).unwrap();
    assert_eq!(font.texture, "fonttex");
    assert_eq!(font.line_height, 10.0);
    assert_eq!(font.fallback, 63);
    assert_eq!(font.glyphs.len(), 4);
    assert_eq!(font.glyphs[&98].source, [0, 4, 4, 4]);
}

#[test]
fn font_glyph_lookup_prefers_exact_codepoints() {
    let font = font_from(&font_json()).unwrap();
    assert_eq!(font.glyph('a').advance, 5.0);
    assert_eq!(font.glyph('b').advance, 6.0);
}

#[test]
fn font_glyph_lookup_falls_back_for_unknown_ascii() {
    let font = font_from(&font_json()).unwrap();
    assert_eq!(font.glyph('z').advance, 4.0);
}

#[test]
fn font_glyph_lookup_falls_back_for_astral_characters() {
    let font = font_from(&font_json()).unwrap();
    assert_eq!(font.glyph('\u{1F600}').advance, 4.0);
}

#[test]
fn font_glyph_lookup_is_case_sensitive() {
    let font = font_from(&font_json()).unwrap();
    assert_eq!(font.glyph('A').advance, font.glyph('?').advance);
    assert_ne!(font.glyph('a').advance, font.glyph('A').advance);
}

#[test]
fn font_validates_against_the_matching_atlas_size() {
    let font = font_from(&font_json()).unwrap();
    font.validate(8, 8).unwrap();
}

#[test]
fn font_validation_fails_when_the_atlas_is_too_small() {
    let font = font_from(&font_json()).unwrap();
    expect_code(font.validate(7, 8), "asset.font.invalid");
}

#[test]
fn font_validation_fails_when_the_atlas_is_too_short() {
    let font = font_from(&font_json()).unwrap();
    expect_code(font.validate(8, 7), "asset.font.invalid");
}

fn one_glyph_atlas(source: [u32; 4], advance: f32, bearing: [f32; 2]) -> FontAtlas {
    FontAtlas {
        texture: "t".into(),
        line_height: 8.0,
        fallback: 63,
        glyphs: BTreeMap::from([(
            63,
            FontGlyph {
                source,
                advance,
                bearing,
            },
        )]),
    }
}

fn glyph_validity(source: [u32; 4], advance: f32, bearing: [f32; 2], ok: bool) {
    let result = one_glyph_atlas(source, advance, bearing).validate(8, 8);
    assert_eq!(result.is_ok(), ok);
}

table! { glyph_validity;
    glyph_inside_atlas_is_valid => ([0, 0, 4, 4], 4.0, [0.0, 0.0], true);
    glyph_touching_atlas_edge_is_valid => ([4, 4, 4, 4], 4.0, [0.0, 0.0], true);
    glyph_one_pixel_past_right_edge_is_invalid => ([5, 0, 4, 4], 4.0, [0.0, 0.0], false);
    glyph_one_pixel_past_bottom_edge_is_invalid => ([0, 5, 4, 4], 4.0, [0.0, 0.0], false);
    glyph_with_negative_advance_is_invalid => ([0, 0, 4, 4], -1.0, [0.0, 0.0], false);
    glyph_with_nan_advance_is_invalid => ([0, 0, 4, 4], f32::NAN, [0.0, 0.0], false);
    glyph_with_infinite_bearing_is_invalid => ([0, 0, 4, 4], 4.0, [f32::INFINITY, 0.0], false);
    glyph_with_nan_bearing_is_invalid => ([0, 0, 4, 4], 4.0, [0.0, f32::NAN], false);
    glyph_with_overflowing_rectangle_is_invalid => ([u32::MAX, 0, 2, 1], 4.0, [0.0, 0.0], false);
    glyph_with_zero_advance_is_valid => ([0, 0, 4, 4], 0.0, [0.0, 0.0], true);
    glyph_with_negative_bearing_is_valid => ([0, 0, 4, 4], 4.0, [-2.0, -3.0], true);
    blank_glyph_rectangle_is_valid => ([0, 0, 0, 0], 3.0, [0.0, 0.0], true);
}

// ---- sprite quads --------------------------------------------------------------

fn tex(width: u32, height: u32) -> Texture {
    Texture {
        width,
        height,
        pixels: vec![255; (width * height * 4) as usize],
    }
}

fn bad_source(source: [u32; 4], width: u32, height: u32) {
    let t = Texture {
        width,
        height,
        pixels: vec![],
    };
    assert_eq!(
        code(sprite_quad(
            [0.0; 3], [1.0; 2], [0.5; 2], source, &t, [1.0; 4]
        )),
        "asset.sprite.rectangle"
    );
}

table! { bad_source;
    sprite_source_beyond_right_edge_is_rejected => ([3, 0, 2, 1], 4, 2);
    sprite_source_beyond_bottom_edge_is_rejected => ([0, 1, 1, 2], 4, 2);
    sprite_source_with_zero_width_is_rejected => ([0, 0, 0, 1], 4, 2);
    sprite_source_with_zero_height_is_rejected => ([0, 0, 1, 0], 4, 2);
    sprite_source_overflowing_origin_is_rejected => ([u32::MAX, 0, 1, 1], 4, 2);
    sprite_source_overflowing_extent_is_rejected => ([0, 0, u32::MAX, 1], 4, 2);
    sprite_source_on_zero_width_texture_is_rejected => ([0, 0, 1, 1], 0, 2);
    sprite_source_on_zero_height_texture_is_rejected => ([0, 0, 1, 1], 2, 0);
}

fn quad_origin(pivot: [f32; 2], expected_x: f32, expected_y: f32) {
    let t = tex(4, 2);
    let mesh = sprite_quad(
        [2.0, 3.0, 5.0],
        [4.0, 6.0],
        pivot,
        [0, 0, 4, 2],
        &t,
        [1.0; 4],
    )
    .unwrap();
    let v = &mesh.vertices;
    assert_eq!(v[0].position, [expected_x, expected_y, 5.0]);
    assert_eq!(v[1].position, [expected_x + 4.0, expected_y, 5.0]);
    assert_eq!(v[2].position, [expected_x + 4.0, expected_y + 6.0, 5.0]);
    assert_eq!(v[3].position, [expected_x, expected_y + 6.0, 5.0]);
}

table! { quad_origin;
    sprite_quad_centered_pivot_offsets_by_half_size => ([0.5, 0.5], 0.0, 0.0);
    sprite_quad_bottom_left_pivot_starts_at_position => ([0.0, 0.0], 2.0, 3.0);
    sprite_quad_top_right_pivot_shifts_by_full_size => ([1.0, 1.0], -2.0, -3.0);
    sprite_quad_mixed_pivot_shifts_each_axis_separately => ([1.0, 0.0], -2.0, 3.0);
}

#[test]
fn sprite_quad_uvs_flip_vertically_for_top_left_atlases() {
    let t = tex(4, 2);
    let mesh = sprite_quad([0.0; 3], [1.0; 2], [0.0; 2], [1, 0, 2, 1], &t, [1.0; 4]).unwrap();
    assert_eq!(mesh.vertices[0].uv, [0.25, 0.5]);
    assert_eq!(mesh.vertices[1].uv, [0.75, 0.5]);
    assert_eq!(mesh.vertices[2].uv, [0.75, 0.0]);
    assert_eq!(mesh.vertices[3].uv, [0.25, 0.0]);
}

#[test]
fn sprite_quad_whole_texture_covers_unit_uv_square() {
    let t = tex(4, 2);
    let mesh = sprite_quad([0.0; 3], [1.0; 2], [0.0; 2], [0, 0, 4, 2], &t, [1.0; 4]).unwrap();
    assert_eq!(mesh.vertices[0].uv, [0.0, 1.0]);
    assert_eq!(mesh.vertices[2].uv, [1.0, 0.0]);
}

#[test]
fn sprite_quad_bottom_row_selects_upper_v_range() {
    let t = tex(4, 2);
    let mesh = sprite_quad([0.0; 3], [1.0; 2], [0.0; 2], [0, 1, 4, 1], &t, [1.0; 4]).unwrap();
    assert_eq!(mesh.vertices[0].uv[1], 1.0);
    assert_eq!(mesh.vertices[2].uv[1], 0.5);
}

#[test]
fn sprite_quad_tint_is_copied_to_every_corner() {
    let t = tex(2, 2);
    let tint = [0.25, 0.5, 0.75, 0.125];
    let mesh = sprite_quad([0.0; 3], [1.0; 2], [0.5; 2], [0, 0, 2, 2], &t, tint).unwrap();
    assert!(mesh.vertices.iter().all(|v| v.color == tint));
}

#[test]
fn sprite_quad_uses_two_counter_clockwise_triangles() {
    let t = tex(2, 2);
    let mesh = sprite_quad([0.0; 3], [1.0; 2], [0.5; 2], [0, 0, 2, 2], &t, [1.0; 4]).unwrap();
    assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
}

// ---- animation frames ----------------------------------------------------------

fn frame(elapsed: f64, fps: f32, count: usize, looping: bool, expected: usize) {
    assert_eq!(animation_frame(elapsed, fps, count, looping), expected);
}

table! { frame;
    animation_starts_on_frame_zero => (0.0, 8.0, 4, true, 0);
    animation_advances_with_elapsed_time => (0.25, 8.0, 4, true, 2);
    animation_looping_wraps_to_the_first_frame => (0.5, 8.0, 4, true, 0);
    animation_looping_wraps_past_the_first_cycle => (0.625, 8.0, 4, true, 1);
    animation_frame_changes_exactly_on_the_boundary => (0.5, 2.0, 4, true, 1);
    animation_just_before_the_boundary_stays => (0.499, 2.0, 4, true, 0);
    animation_once_mode_holds_the_last_frame => (0.5, 8.0, 4, false, 3);
    animation_once_mode_reaches_intermediate_frames => (0.125, 8.0, 4, false, 1);
    animation_once_mode_clamps_after_a_long_time => (1.0e6, 12.0, 6, false, 5);
    animation_long_loop_stays_deterministic => (1000.0, 60.0, 7, true, 3);
    animation_zero_fps_is_static => (5.0, 0.0, 4, true, 0);
    animation_negative_fps_is_static => (5.0, -4.0, 4, true, 0);
    animation_nan_fps_is_static => (5.0, f32::NAN, 4, true, 0);
    animation_infinite_fps_is_static => (5.0, f32::INFINITY, 4, true, 0);
    animation_negative_time_is_frame_zero => (-1.0, 8.0, 4, true, 0);
    animation_nan_time_is_frame_zero => (f64::NAN, 8.0, 4, true, 0);
    animation_infinite_time_is_frame_zero => (f64::INFINITY, 8.0, 4, true, 0);
    animation_without_frames_is_frame_zero => (3.0, 8.0, 0, true, 0);
    animation_single_frame_loops_on_itself => (9.0, 8.0, 1, true, 0);
    animation_single_frame_once_mode_stays => (9.0, 8.0, 1, false, 0);
}

// ---- letterboxing and pointer mapping ----------------------------------------

fn viewport(logical: [u32; 2], fb: [u32; 2], expected: [f32; 5]) {
    let v = camera_viewport(logical, fb);
    assert_eq!([v.x, v.y, v.width, v.height, v.scale], expected);
}

table! { viewport;
    viewport_exact_fit_has_no_bars => ([320, 180], [320, 180], [0.0, 0.0, 320.0, 180.0, 1.0]);
    viewport_integer_double_scale_fills_the_window => ([320, 180], [640, 360], [0.0, 0.0, 640.0, 360.0, 2.0]);
    viewport_fractional_fit_rounds_down_to_whole_scale => ([320, 180], [1000, 700], [20.0, 80.0, 960.0, 540.0, 3.0]);
    viewport_small_window_scales_down_fractionally => ([320, 180], [160, 90], [0.0, 0.0, 160.0, 90.0, 0.5]);
    viewport_narrow_window_letterboxes_vertically => ([320, 180], [200, 200], [0.0, 43.0, 200.0, 112.5, 0.625]);
    viewport_barely_larger_window_keeps_scale_one => ([320, 180], [330, 190], [5.0, 5.0, 320.0, 180.0, 1.0]);
    viewport_one_pixel_extra_width_floors_the_margin => ([320, 180], [641, 360], [0.0, 0.0, 640.0, 360.0, 2.0]);
    viewport_square_logical_in_wide_window => ([100, 100], [1920, 1080], [460.0, 40.0, 1000.0, 1000.0, 10.0]);
    viewport_zero_logical_width_collapses => ([0, 180], [640, 360], [0.0, 0.0, 0.0, 0.0, 0.0]);
    viewport_zero_logical_height_collapses => ([320, 0], [640, 360], [0.0, 0.0, 0.0, 0.0, 0.0]);
    viewport_zero_framebuffer_width_collapses => ([320, 180], [0, 360], [0.0, 0.0, 0.0, 0.0, 0.0]);
    viewport_zero_framebuffer_height_collapses => ([320, 180], [640, 0], [0.0, 0.0, 0.0, 0.0, 0.0]);
}

fn pointer(p: [f32; 2], window: [u32; 2], fb: [u32; 2], logical: [u32; 2], expected: [f32; 2]) {
    let got = logical_pointer(p, window, fb, logical);
    assert!(
        near(got[0], expected[0]) && near(got[1], expected[1]),
        "{got:?} vs {expected:?}"
    );
}

table! { pointer;
    pointer_at_window_center_maps_to_logical_center => ([250.0, 175.0], [500, 350], [1000, 700], [320, 180], [160.0, 90.0]);
    pointer_at_viewport_origin_maps_to_zero => ([20.0, 80.0], [1000, 700], [1000, 700], [320, 180], [0.0, 0.0]);
    pointer_inside_viewport_maps_through_the_scale => ([50.0, 95.0], [1000, 700], [1000, 700], [320, 180], [10.0, 5.0]);
    pointer_in_the_left_bar_is_negative => ([0.0, 350.0], [1000, 700], [1000, 700], [320, 180], [-20.0 / 3.0, 90.0]);
    pointer_in_the_right_bar_exceeds_logical_width => ([1000.0, 350.0], [1000, 700], [1000, 700], [320, 180], [980.0 / 3.0, 90.0]);
    pointer_scales_from_window_to_framebuffer_units => ([60.0, 95.0], [500, 350], [1000, 700], [320, 180], [100.0 / 3.0, 110.0 / 3.0]);
    pointer_with_identical_window_and_framebuffer_needs_no_scaling => ([660.0, 350.0], [640, 360], [640, 360], [320, 180], [330.0, 175.0]);
}

fn pointer_invalid(p: [f32; 2], window: [u32; 2], fb: [u32; 2], logical: [u32; 2]) {
    assert_eq!(logical_pointer(p, window, fb, logical), [-1.0, -1.0]);
}

table! { pointer_invalid;
    pointer_with_zero_window_is_unmapped => ([1.0, 1.0], [0, 350], [640, 360], [320, 180]);
    pointer_with_zero_window_height_is_unmapped => ([1.0, 1.0], [500, 0], [640, 360], [320, 180]);
    pointer_with_nan_x_is_unmapped => ([f32::NAN, 1.0], [640, 360], [640, 360], [320, 180]);
    pointer_with_infinite_y_is_unmapped => ([1.0, f32::INFINITY], [640, 360], [640, 360], [320, 180]);
    pointer_with_collapsed_viewport_is_unmapped => ([1.0, 1.0], [640, 360], [0, 0], [320, 180]);
    pointer_with_zero_logical_size_is_unmapped => ([1.0, 1.0], [640, 360], [640, 360], [0, 0]);
}

// ---- text geometry ---------------------------------------------------------------

fn test_font() -> FontAtlas {
    let glyph = |source, advance, bearing| FontGlyph {
        source,
        advance,
        bearing,
    };
    FontAtlas {
        texture: "t".into(),
        line_height: 5.0,
        fallback: 63,
        glyphs: BTreeMap::from([
            (63, glyph([0, 0, 1, 1], 1.0, [0.0, 0.0])),
            (97, glyph([0, 0, 2, 2], 3.0, [0.0, 0.0])),
            (98, glyph([2, 0, 2, 2], 4.0, [1.0, 1.0])),
            (32, glyph([0, 0, 0, 0], 2.0, [0.0, 0.0])),
        ]),
    }
}

fn text(s: &str, origin: [f32; 2], wrap: f32, scale: f32) -> Mesh {
    text_mesh(&test_font(), &tex(4, 4), s, origin, wrap, scale, [1.0; 4]).unwrap()
}

fn first_corner(mesh: &Mesh, quad: usize) -> [f32; 2] {
    let v = mesh.vertices[quad * 4].position;
    [v[0], v[1]]
}

#[test]
fn text_single_glyph_is_a_quad_below_the_origin_in_ui_space() {
    let mesh = text("a", [10.0, 20.0], 0.0, 1.0);
    let corners: Vec<_> = mesh
        .vertices
        .iter()
        .map(|v| [v.position[0], v.position[1]])
        .collect();
    assert_eq!(
        corners,
        vec![[10.0, 22.0], [12.0, 22.0], [12.0, 20.0], [10.0, 20.0]]
    );
    assert_eq!(mesh.vertices[0].uv, [0.0, 0.5]);
}

#[test]
fn text_glyph_bearing_offsets_the_quad_downwards_and_rightwards() {
    let mesh = text("ab", [0.0; 2], 0.0, 1.0);
    assert_eq!(first_corner(&mesh, 1), [4.0, 3.0]);
}

#[test]
fn text_pen_advances_by_the_glyph_advance() {
    let mesh = text("aa", [0.0; 2], 0.0, 1.0);
    assert_eq!(first_corner(&mesh, 1)[0], 3.0);
}

#[test]
fn text_scale_multiplies_advance_and_quad_size() {
    let mesh = text("aa", [0.0; 2], 0.0, 2.0);
    assert_eq!(first_corner(&mesh, 1)[0], 6.0);
    assert_eq!(
        mesh.vertices[2].position[0] - mesh.vertices[3].position[0],
        4.0
    );
}

#[test]
fn text_newline_moves_down_by_the_scaled_line_height() {
    let mesh = text("a\na", [0.0; 2], 0.0, 1.0);
    assert_eq!(first_corner(&mesh, 1), [0.0, 7.0]);
}

#[test]
fn text_newline_scales_with_the_text_scale() {
    let mesh = text("a\na", [0.0; 2], 0.0, 2.0);
    assert_eq!(first_corner(&mesh, 1), [0.0, 14.0]);
}

#[test]
fn text_carriage_returns_are_dropped() {
    assert_eq!(
        text("a\r\na", [0.0; 2], 0.0, 1.0).vertices,
        text("a\na", [0.0; 2], 0.0, 1.0).vertices
    );
}

#[test]
fn text_blank_glyph_advances_without_a_quad() {
    let mesh = text("a a", [0.0; 2], 0.0, 1.0);
    assert_eq!(mesh.vertices.len(), 8);
    assert_eq!(first_corner(&mesh, 1)[0], 5.0);
}

#[test]
fn text_empty_string_produces_an_empty_mesh() {
    let mesh = text("", [0.0; 2], 0.0, 1.0);
    assert!(mesh.vertices.is_empty() && mesh.indices.is_empty());
}

#[test]
fn text_unknown_character_uses_the_fallback_glyph() {
    let mesh = text("z", [0.0; 2], 0.0, 1.0);
    assert_eq!(mesh.vertices.len(), 4);
    assert_eq!(
        mesh.vertices[2].position[0] - mesh.vertices[3].position[0],
        1.0
    );
}

#[test]
fn text_astral_character_uses_the_fallback_glyph_once() {
    let mesh = text("\u{1F600}", [0.0; 2], 0.0, 1.0);
    assert_eq!(mesh.vertices.len(), 4);
}

#[test]
fn text_indices_are_offset_per_glyph() {
    let mesh = text("aa", [0.0; 2], 0.0, 1.0);
    assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
}

#[test]
fn text_wraps_whole_words_before_splitting_glyphs() {
    let mesh = text("ab ab", [0.0; 2], 10.0, 1.0);
    assert_eq!(mesh.vertices.len(), 16);
    assert_eq!(first_corner(&mesh, 1), [4.0, 3.0]);
    assert_eq!(first_corner(&mesh, 2), [0.0, 7.0]);
    assert_eq!(first_corner(&mesh, 3), [4.0, 8.0]);
}

#[test]
fn text_trailing_space_that_overflows_hangs_instead_of_wrapping_onto_a_blank_line() {
    // "ab" fills 7 of 8 columns; the following space hangs and the next word wraps once.
    let mesh = text("ab ab", [0.0; 2], 8.0, 1.0);
    assert_eq!(mesh.vertices.len(), 16);
    assert_eq!(first_corner(&mesh, 2), [0.0, 7.0]);
    assert_eq!(first_corner(&mesh, 3), [4.0, 8.0]);
}

#[test]
fn text_explicit_newline_after_an_overflowing_space_adds_exactly_one_line() {
    let mesh = text(
        "ab 
ab", [0.0; 2], 8.0, 1.0,
    );
    assert_eq!(first_corner(&mesh, 2), [0.0, 7.0]);
}

#[test]
fn text_long_word_breaks_between_glyphs_at_the_wrap_width() {
    let mesh = text("aaaa", [0.0; 2], 7.0, 1.0);
    assert_eq!(first_corner(&mesh, 1), [3.0, 2.0]);
    assert_eq!(first_corner(&mesh, 2), [0.0, 7.0]);
    assert_eq!(first_corner(&mesh, 3), [3.0, 7.0]);
}

#[test]
fn text_without_wrap_width_never_wraps() {
    let mesh = text("aaaaaaaa", [0.0; 2], 0.0, 1.0);
    assert!((0..8).all(|i| first_corner(&mesh, i)[1] == 2.0));
}

#[test]
fn text_wrap_is_measured_from_the_origin() {
    let mesh = text("aaaa", [100.0, 0.0], 7.0, 1.0);
    assert_eq!(first_corner(&mesh, 2), [100.0, 7.0]);
}

fn bad_text(s: &str, wrap: f32, scale: f32) {
    let result = text_mesh(&test_font(), &tex(4, 4), s, [0.0; 2], wrap, scale, [1.0; 4]);
    assert_eq!(code(result), "ui.text.invalid");
}

table! { bad_text;
    text_zero_scale_is_invalid => ("a", 0.0, 0.0);
    text_negative_scale_is_invalid => ("a", 0.0, -1.0);
    text_nan_scale_is_invalid => ("a", 0.0, f32::NAN);
    text_infinite_scale_is_invalid => ("a", 0.0, f32::INFINITY);
    text_negative_wrap_is_invalid => ("a", -1.0, 1.0);
    text_nan_wrap_is_invalid => ("a", f32::NAN, 1.0);
}

#[test]
fn text_longer_than_64_kibibytes_is_refused() {
    let result = text_mesh(
        &test_font(),
        &tex(4, 4),
        &"a".repeat(65537),
        [0.0; 2],
        0.0,
        1.0,
        [1.0; 4],
    );
    assert_eq!(code(result), "ui.text.invalid");
}

#[test]
fn text_of_exactly_64_kibibytes_is_accepted() {
    let mesh = text_mesh(
        &test_font(),
        &tex(4, 4),
        &"a".repeat(65536),
        [0.0; 2],
        0.0,
        1.0,
        [1.0; 4],
    )
    .unwrap();
    assert_eq!(mesh.vertices.len(), 65536 * 4);
}

#[test]
fn text_glyph_outside_the_texture_reports_the_sprite_rectangle() {
    let mut font = test_font();
    font.glyphs.get_mut(&97).unwrap().source = [3, 3, 2, 2];
    let result = text_mesh(&font, &tex(4, 4), "a", [0.0; 2], 0.0, 1.0, [1.0; 4]);
    assert_eq!(code(result), "asset.sprite.rectangle");
}

#[test]
fn text_color_is_applied_to_every_vertex() {
    let color = [0.5, 0.25, 1.0, 0.75];
    let mesh = text_mesh(&test_font(), &tex(4, 4), "ab", [0.0; 2], 0.0, 1.0, color).unwrap();
    assert!(mesh.vertices.iter().all(|v| v.color == color));
}
