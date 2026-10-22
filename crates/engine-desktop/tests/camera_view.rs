//! Pure CPU contract for the logical viewport, pixel snapping, projection
//! matrices, scissor conversion and pointer mapping. No device is created.

use engine_desktop::{CameraView, PixelRect};
use engine_project::Camera2D;

/// Default camera: 320x180 logical pixels, 16 pixels per world unit.
fn default_camera() -> Camera2D {
    Camera2D::default()
}

fn camera(logical: [u32; 2], ppu: f32, center: [f32; 2], snap: bool) -> Camera2D {
    Camera2D {
        logical_size: logical,
        pixels_per_unit: ppu,
        center,
        pixel_snap: snap,
        ..Default::default()
    }
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect {
        x,
        y,
        width,
        height,
    }
}

/// Column-major matrix times (x, y, z, 1).
fn apply(m: &[f32; 16], p: [f32; 3]) -> [f32; 4] {
    std::array::from_fn(|row| m[row] * p[0] + m[4 + row] * p[1] + m[8 + row] * p[2] + m[12 + row])
}

fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-5, "{a} != {b}");
}

fn close2(a: [f32; 2], b: [f32; 2]) {
    close(a[0], b[0]);
    close(a[1], b[1]);
}

macro_rules! viewport_cases {
    ($($name:ident: ($w:expr, $h:expr) => $scale:expr, $rect:expr;)*) => {$(
        #[test]
        fn $name() {
            let view = CameraView::new(&default_camera(), $w, $h).unwrap();
            assert_eq!(view.scale, $scale);
            assert_eq!(view.viewport, $rect);
        }
    )*};
}

viewport_cases! {
    window_equal_to_logical_size_is_one_to_one: (320, 180) => 1.0, rect(0, 0, 320, 180);
    window_double_logical_size_scales_by_two: (640, 360) => 2.0, rect(0, 0, 640, 360);
    full_hd_window_scales_by_six: (1920, 1080) => 6.0, rect(0, 0, 1920, 1080);
    uhd_window_scales_by_twelve: (3840, 2160) => 12.0, rect(0, 0, 3840, 2160);
    non_integer_fit_rounds_scale_down_and_centres: (1000, 700) => 3.0, rect(20, 80, 960, 540);
    height_limited_window_is_letterboxed: (800, 600) => 2.0, rect(80, 120, 640, 360);
    narrow_tall_window_keeps_scale_one_and_centres_vertically: (500, 1000) => 1.0, rect(90, 410, 320, 180);
    sixteen_ten_window_is_letterboxed_top_and_bottom: (1920, 1200) => 6.0, rect(0, 60, 1920, 1080);
    ultrawide_window_is_pillarboxed: (2560, 1080) => 6.0, rect(320, 0, 1920, 1080);
    one_extra_pixel_does_not_change_scale_and_centres_by_floor: (321, 181) => 1.0, rect(0, 0, 320, 180);
    odd_margins_round_down: (323, 183) => 1.0, rect(1, 1, 320, 180);
    half_size_window_uses_fractional_scale: (160, 90) => 0.5, rect(0, 0, 160, 90);
    small_narrow_window_uses_fractional_scale_and_floors_height: (200, 300) => 0.625, rect(0, 94, 200, 112);
    tiny_square_window_uses_fractional_scale: (100, 100) => 0.3125, rect(0, 22, 100, 56);
    one_pixel_window_clamps_viewport_to_one_pixel: (1, 1) => 1.0 / 320.0, rect(0, 0, 1, 1);
}

#[test]
fn zero_sized_window_produces_an_empty_viewport() {
    let view = CameraView::new(&default_camera(), 0, 0).unwrap();
    assert_eq!(view.scale, 0.0);
    assert_eq!((view.viewport.width, view.viewport.height), (0, 0));
}

#[test]
fn zero_width_window_has_no_drawable_viewport_even_with_height() {
    let view = CameraView::new(&default_camera(), 0, 100).unwrap();
    assert_eq!(view.scale, 0.0);
    assert_eq!(view.viewport.width, 0);
    assert_eq!(view.scissor(None).unwrap(), None);
}

#[test]
fn custom_logical_size_uses_its_own_aspect_for_the_fit() {
    let view = CameraView::new(&camera([64, 64], 1.0, [0.0; 2], true), 100, 100).unwrap();
    assert_eq!(view.viewport, rect(18, 18, 64, 64));
    assert_eq!(view.scale, 1.0);
}

#[test]
fn viewport_never_exceeds_the_window() {
    for (w, h) in [
        (1, 1),
        (7, 3),
        (319, 179),
        (320, 179),
        (333, 777),
        (4096, 17),
        (2, 2000),
    ] {
        let v = CameraView::new(&default_camera(), w, h).unwrap().viewport;
        assert!(v.x + v.width <= w, "{w}x{h}: {v:?}");
        assert!(v.y + v.height <= h, "{w}x{h}: {v:?}");
    }
}

#[test]
fn integer_scales_never_distort_the_logical_aspect_ratio() {
    for k in 1..=9u32 {
        let v = CameraView::new(&default_camera(), 320 * k + 5, 180 * k + 9).unwrap();
        assert_eq!(v.scale, f64::from(k));
        assert_eq!((v.viewport.width, v.viewport.height), (320 * k, 180 * k));
    }
}

#[test]
fn viewport_is_centred_within_one_pixel() {
    for (w, h) in [(1000, 700), (801, 603), (1919, 1081), (555, 777)] {
        let v = CameraView::new(&default_camera(), w, h).unwrap().viewport;
        let (left, right) = (v.x, w - v.x - v.width);
        let (top, bottom) = (v.y, h - v.y - v.height);
        assert!(
            right - left <= 1,
            "{w}x{h} horizontal margins {left}/{right}"
        );
        assert!(bottom - top <= 1, "{w}x{h} vertical margins {top}/{bottom}");
    }
}

// ---- projection matrices ---------------------------------------------------------------

#[test]
fn world_origin_projects_to_screen_centre() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    assert_eq!(apply(&view.world, [0.0; 3]), [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn world_half_extent_is_half_the_logical_size_over_ppu() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    // 320 / 16 / 2 = 10 world units horizontally, 180 / 16 / 2 = 5.625 vertically.
    close(apply(&view.world, [10.0, 0.0, 0.0])[0], 1.0);
    close(apply(&view.world, [-10.0, 0.0, 0.0])[0], -1.0);
    close(apply(&view.world, [0.0, 5.625, 0.0])[1], 1.0);
    close(apply(&view.world, [0.0, -5.625, 0.0])[1], -1.0);
}

#[test]
fn world_y_points_up() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    assert!(apply(&view.world, [0.0, 1.0, 0.0])[1] > 0.0);
    assert!(apply(&view.world, [1.0, 0.0, 0.0])[0] > 0.0);
}

#[test]
fn camera_centre_is_subtracted_before_projection() {
    let view = CameraView::new(&camera([320, 180], 16.0, [3.0, -2.0], true), 640, 360).unwrap();
    close2(
        [
            apply(&view.world, [3.0, -2.0, 0.0])[0],
            apply(&view.world, [3.0, -2.0, 0.0])[1],
        ],
        [0.0, 0.0],
    );
    close(apply(&view.world, [13.0, -2.0, 0.0])[0], 1.0);
    close(apply(&view.world, [3.0, 3.625, 0.0])[1], 1.0);
}

#[test]
fn doubling_pixels_per_unit_halves_the_visible_world() {
    let view = CameraView::new(&camera([320, 180], 32.0, [0.0; 2], true), 640, 360).unwrap();
    close(apply(&view.world, [5.0, 0.0, 0.0])[0], 1.0);
}

#[test]
fn depth_passes_through_the_world_matrix_unchanged() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    let p = apply(&view.world, [1.0, 2.0, 0.375]);
    assert_eq!((p[2], p[3]), (0.375, 1.0));
}

#[test]
fn world_matrix_is_axis_aligned_with_no_shear() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    for (i, v) in view.world.iter().enumerate() {
        if ![0, 5, 10, 12, 13, 15].contains(&i) {
            assert_eq!(*v, 0.0, "element {i}");
        }
    }
    assert_eq!(view.world[10], 1.0);
    assert_eq!(view.world[15], 1.0);
}

#[test]
fn square_logical_size_projects_equally_on_both_axes() {
    let view = CameraView::new(&camera([64, 64], 8.0, [0.0; 2], true), 128, 128).unwrap();
    assert_eq!(view.world[0], view.world[5]);
    close(apply(&view.world, [4.0, 4.0, 0.0])[0], 1.0);
    close(apply(&view.world, [4.0, 4.0, 0.0])[1], 1.0);
}

#[test]
fn ui_matrix_maps_top_left_to_minus_one_plus_one() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    close2(sub2(apply(&view.ui, [0.0, 0.0, 0.0])), [-1.0, 1.0]);
}

#[test]
fn ui_matrix_maps_bottom_right_to_plus_one_minus_one() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    close2(sub2(apply(&view.ui, [320.0, 180.0, 0.0])), [1.0, -1.0]);
}

#[test]
fn ui_matrix_maps_the_logical_centre_to_the_origin() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    close2(sub2(apply(&view.ui, [160.0, 90.0, 0.0])), [0.0, 0.0]);
}

#[test]
fn ui_y_points_down() {
    let view = CameraView::new(&default_camera(), 640, 360).unwrap();
    let top = apply(&view.ui, [100.0, 10.0, 0.0])[1];
    let lower = apply(&view.ui, [100.0, 100.0, 0.0])[1];
    assert!(lower < top);
}

#[test]
fn ui_matrix_ignores_camera_centre_and_pixels_per_unit() {
    let plain = CameraView::new(&default_camera(), 640, 360).unwrap();
    let moved = CameraView::new(&camera([320, 180], 99.0, [40.0, 40.0], false), 640, 360).unwrap();
    assert_eq!(plain.ui, moved.ui);
}

#[test]
fn ui_matrix_follows_logical_size_not_window_size() {
    let small = CameraView::new(&default_camera(), 320, 180).unwrap();
    let large = CameraView::new(&default_camera(), 1920, 1080).unwrap();
    assert_eq!(small.ui, large.ui);
    let other = CameraView::new(&camera([64, 32], 1.0, [0.0; 2], true), 64, 32).unwrap();
    close2(sub2(apply(&other.ui, [64.0, 32.0, 0.0])), [1.0, -1.0]);
}

fn sub2(p: [f32; 4]) -> [f32; 2] {
    [p[0], p[1]]
}

// ---- pixel snapping ----------------------------------------------------------------------

fn translation(view: &CameraView) -> [f32; 2] {
    [view.world[12], view.world[13]]
}

#[test]
fn centre_below_half_a_pixel_snaps_back_to_zero() {
    // 0.03 world units = 0.48 px.
    let view = CameraView::new(&camera([320, 180], 16.0, [0.03, 0.03], true), 320, 180).unwrap();
    assert_eq!(translation(&view), [0.0, 0.0]);
}

#[test]
fn centre_above_half_a_pixel_snaps_to_the_next_pixel() {
    // 0.04 world units = 0.64 px -> 1 px = 0.0625 units; sx = 0.1.
    let view = CameraView::new(&camera([320, 180], 16.0, [0.04, 0.04], true), 320, 180).unwrap();
    close(view.world[12], -0.0625 * 0.1);
    close(view.world[13], -0.0625 * (2.0 * 16.0 / 180.0));
}

#[test]
fn negative_centre_snaps_symmetrically() {
    let view = CameraView::new(&camera([320, 180], 16.0, [-0.04, -0.04], true), 320, 180).unwrap();
    close(view.world[12], 0.0625 * 0.1);
}

#[test]
fn exact_half_pixel_rounds_away_from_zero() {
    let positive =
        CameraView::new(&camera([320, 180], 16.0, [0.031_25, 0.0], true), 320, 180).unwrap();
    let negative =
        CameraView::new(&camera([320, 180], 16.0, [-0.031_25, 0.0], true), 320, 180).unwrap();
    close(positive.world[12], -0.0625 * 0.1);
    close(negative.world[12], 0.0625 * 0.1);
}

#[test]
fn centre_already_on_the_pixel_grid_is_unchanged() {
    let view = CameraView::new(&camera([320, 180], 16.0, [1.25, -0.5], true), 320, 180).unwrap();
    close(view.world[12], -1.25 * 0.1);
    close(view.world[13], 0.5 * (2.0 * 16.0 / 180.0));
}

#[test]
fn disabling_snap_keeps_sub_pixel_camera_motion() {
    let view = CameraView::new(&camera([320, 180], 16.0, [0.04, 0.0], false), 320, 180).unwrap();
    close(view.world[12], -0.04 * 0.1);
}

#[test]
fn snapping_happens_in_source_pixel_space_not_window_pixels() {
    let small = CameraView::new(&camera([320, 180], 16.0, [0.04, 0.0], true), 320, 180).unwrap();
    let large = CameraView::new(&camera([320, 180], 16.0, [0.04, 0.0], true), 1920, 1080).unwrap();
    assert_eq!(small.world, large.world);
}

#[test]
fn snapping_with_one_pixel_per_unit_rounds_to_whole_units() {
    let low = CameraView::new(&camera([100, 100], 1.0, [2.4, 0.0], true), 100, 100).unwrap();
    let high = CameraView::new(&camera([100, 100], 1.0, [2.6, 0.0], true), 100, 100).unwrap();
    close(low.world[12], -2.0 * 0.02);
    close(high.world[12], -3.0 * 0.02);
}

#[test]
fn snapped_camera_moves_geometry_by_whole_source_pixels() {
    let a = CameraView::new(&camera([320, 180], 16.0, [0.0, 0.0], true), 320, 180).unwrap();
    let b = CameraView::new(&camera([320, 180], 16.0, [0.07, 0.0], true), 320, 180).unwrap();
    // Origin shifts by exactly one source pixel: 2 / 320 in NDC.
    let shift = apply(&a.world, [0.0; 3])[0] - apply(&b.world, [0.0; 3])[0];
    close(shift, 2.0 / 320.0);
}

// ---- construction errors ---------------------------------------------------------------

macro_rules! rejects_camera {
    ($($name:ident: $camera:expr, $message:expr;)*) => {$(
        #[test]
        fn $name() {
            let camera: Camera2D = $camera;
            let error = CameraView::new(&camera, 640, 360).unwrap_err();
            assert!(error.to_string().contains($message), "{error}");
        }
    )*};
}

rejects_camera! {
    rejects_zero_logical_width: camera([0, 180], 16.0, [0.0; 2], true), "logical size";
    rejects_zero_logical_height: camera([320, 0], 16.0, [0.0; 2], true), "logical size";
    rejects_zero_pixels_per_unit: camera([320, 180], 0.0, [0.0; 2], true), "pixels per unit";
    rejects_negative_pixels_per_unit: camera([320, 180], -16.0, [0.0; 2], true), "pixels per unit";
    rejects_nan_pixels_per_unit: camera([320, 180], f32::NAN, [0.0; 2], true), "pixels per unit";
    rejects_infinite_pixels_per_unit: camera([320, 180], f32::INFINITY, [0.0; 2], true), "pixels per unit";
    rejects_nan_centre_x: camera([320, 180], 16.0, [f32::NAN, 0.0], true), "camera center";
    rejects_infinite_centre_y: camera([320, 180], 16.0, [0.0, f32::NEG_INFINITY], true), "camera center";
    rejects_matrix_that_overflows_f32: camera([320, 180], 1.0e38, [1.0e10, 0.0], false), "finite range";
}

#[test]
fn matrix_overflow_is_detected_with_snapping_enabled_too() {
    let camera = camera([320, 180], 1.0e38, [1.0e10, 0.0], true);
    assert!(CameraView::new(&camera, 640, 360).is_err());
}

#[test]
fn errors_do_not_depend_on_window_size() {
    let bad = camera([0, 0], 16.0, [0.0; 2], true);
    for (w, h) in [(0, 0), (1, 1), (4000, 4000)] {
        assert!(CameraView::new(&bad, w, h).is_err());
    }
}

// ---- scissor ---------------------------------------------------------------------------

/// Window 1000x700 with the default camera: scale 3, viewport (20, 80, 960, 540).
fn letterboxed() -> CameraView {
    CameraView::new(&default_camera(), 1000, 700).unwrap()
}

#[test]
fn no_clip_scissors_to_the_whole_viewport() {
    assert_eq!(
        letterboxed().scissor(None).unwrap(),
        Some(rect(20, 80, 960, 540))
    );
}

#[test]
fn clip_is_scaled_and_offset_into_the_viewport() {
    let s = letterboxed().scissor(Some([0.0, 0.0, 10.0, 10.0])).unwrap();
    assert_eq!(s, Some(rect(20, 80, 30, 30)));
}

#[test]
fn fractional_clip_edges_expand_outwards_to_whole_pixels() {
    let s = letterboxed().scissor(Some([10.5, 5.5, 2.2, 3.1])).unwrap();
    assert_eq!(s, Some(rect(51, 96, 8, 10)));
}

#[test]
fn clip_overlapping_the_top_left_is_cut_at_the_viewport_origin() {
    let s = letterboxed()
        .scissor(Some([-10.0, -10.0, 20.0, 20.0]))
        .unwrap();
    assert_eq!(s, Some(rect(20, 80, 30, 30)));
}

#[test]
fn clip_overlapping_the_bottom_right_is_cut_at_the_viewport_edge() {
    let s = letterboxed()
        .scissor(Some([315.0, 170.0, 20.0, 20.0]))
        .unwrap();
    assert_eq!(s, Some(rect(20 + 945, 80 + 510, 15, 30)));
}

#[test]
fn clip_fully_left_of_the_viewport_is_culled() {
    assert_eq!(
        letterboxed()
            .scissor(Some([-50.0, 0.0, 10.0, 10.0]))
            .unwrap(),
        None
    );
}

#[test]
fn clip_fully_beyond_the_right_edge_is_culled() {
    assert_eq!(
        letterboxed()
            .scissor(Some([400.0, 0.0, 10.0, 10.0]))
            .unwrap(),
        None
    );
}

#[test]
fn clip_fully_below_the_viewport_is_culled() {
    assert_eq!(
        letterboxed()
            .scissor(Some([0.0, 200.0, 10.0, 10.0]))
            .unwrap(),
        None
    );
}

#[test]
fn clip_larger_than_the_viewport_is_clamped_to_it() {
    let s = letterboxed()
        .scissor(Some([-100.0, -100.0, 1000.0, 1000.0]))
        .unwrap();
    assert_eq!(s, Some(rect(20, 80, 960, 540)));
}

#[test]
fn zero_width_or_height_clip_is_culled_not_unclipped() {
    let v = letterboxed();
    assert_eq!(v.scissor(Some([5.0, 5.0, 0.0, 10.0])).unwrap(), None);
    assert_eq!(v.scissor(Some([5.0, 5.0, 10.0, 0.0])).unwrap(), None);
}

macro_rules! invalid_clip {
    ($($name:ident: $clip:expr;)*) => {$(
        #[test]
        fn $name() {
            let error = letterboxed().scissor(Some($clip)).unwrap_err();
            assert!(error.to_string().contains("invalid draw clip"), "{error}");
        }
    )*};
}

invalid_clip! {
    scissor_rejects_negative_width: [0.0, 0.0, -1.0, 5.0];
    scissor_rejects_negative_height: [0.0, 0.0, 5.0, -0.5];
    scissor_rejects_nan_origin: [f32::NAN, 0.0, 5.0, 5.0];
    scissor_rejects_infinite_width: [0.0, 0.0, f32::INFINITY, 5.0];
    scissor_rejects_infinite_origin: [0.0, f32::NEG_INFINITY, 5.0, 5.0];
}

#[test]
fn empty_viewport_culls_everything_without_validating_the_clip() {
    let empty = CameraView::new(&default_camera(), 0, 0).unwrap();
    assert_eq!(empty.scissor(Some([0.0, 0.0, 5.0, 5.0])).unwrap(), None);
    assert_eq!(
        empty.scissor(Some([f32::NAN, 0.0, 5.0, 5.0])).unwrap(),
        None
    );
}

#[test]
fn sub_pixel_clip_at_native_scale_covers_one_whole_pixel() {
    let view = CameraView::new(&default_camera(), 320, 180).unwrap();
    let s = view.scissor(Some([0.1, 0.1, 0.1, 0.1])).unwrap();
    assert_eq!(s, Some(rect(0, 0, 1, 1)));
}

#[test]
fn fractional_scale_rounds_scissor_edges_outwards() {
    let view = CameraView::new(&default_camera(), 160, 90).unwrap();
    let s = view.scissor(Some([0.0, 0.0, 3.0, 3.0])).unwrap();
    assert_eq!(s, Some(rect(0, 0, 2, 2)));
}

#[test]
fn scissor_is_always_inside_the_viewport() {
    let view = letterboxed();
    let v = view.viewport;
    for clip in [
        [-5.0, -5.0, 400.0, 400.0],
        [100.0, 100.0, 50.5, 50.5],
        [310.0, 170.0, 30.0, 30.0],
        [0.0, 0.0, 320.0, 180.0],
    ] {
        let s = view.scissor(Some(clip)).unwrap().unwrap();
        assert!(s.x >= v.x && s.y >= v.y);
        assert!(s.x + s.width <= v.x + v.width);
        assert!(s.y + s.height <= v.y + v.height);
    }
}

// ---- pointer mapping -------------------------------------------------------------------

#[test]
fn pointer_on_the_viewport_origin_is_logical_zero() {
    assert_eq!(letterboxed().logical_pointer([20.0, 80.0]), [0.0, 0.0]);
}

#[test]
fn pointer_on_the_far_viewport_corner_is_the_logical_size() {
    close2(
        letterboxed().logical_pointer([980.0, 620.0]),
        [320.0, 180.0],
    );
}

#[test]
fn pointer_in_the_middle_maps_to_the_logical_centre() {
    close2(letterboxed().logical_pointer([500.0, 350.0]), [160.0, 90.0]);
}

#[test]
fn pointer_in_the_letterbox_maps_outside_the_logical_area() {
    let p = letterboxed().logical_pointer([0.0, 0.0]);
    assert!(p[0] < 0.0 && p[1] < 0.0);
    close2(p, [-20.0 / 3.0, -80.0 / 3.0]);
}

#[test]
fn pointer_divides_by_fractional_scale() {
    let view = CameraView::new(&default_camera(), 160, 90).unwrap();
    close2(view.logical_pointer([80.0, 45.0]), [160.0, 90.0]);
}

#[test]
fn pointer_with_an_empty_viewport_maps_to_zero() {
    let view = CameraView::new(&default_camera(), 0, 0).unwrap();
    assert_eq!(view.logical_pointer([123.0, 456.0]), [0.0, 0.0]);
}

#[test]
fn pointer_round_trips_through_scale_and_offset() {
    let view = letterboxed();
    for logical in [[0.0f32, 0.0], [10.5, 3.25], [319.0, 179.0], [160.0, 90.0]] {
        let physical = [
            view.viewport.x as f32 + logical[0] * view.scale as f32,
            view.viewport.y as f32 + logical[1] * view.scale as f32,
        ];
        close2(view.logical_pointer(physical), logical);
    }
}

#[test]
fn pixel_rect_default_is_the_empty_origin_rect() {
    assert_eq!(PixelRect::default(), rect(0, 0, 0, 0));
}
