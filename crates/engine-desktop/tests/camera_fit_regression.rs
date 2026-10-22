//! Fractional fits must fill the limiting window axis exactly. `scale` is a
//! quotient such as 98 / 180, so `logical * scale` can land a hair below the
//! integer window size (97.99999999999999) and a plain floor loses a pixel.

use engine_desktop::CameraView;
use engine_project::Camera2D;

#[test]
fn fractional_fit_fills_a_height_limited_window_exactly() {
    let camera = Camera2D::default(); // 320x180
    for height in 1..180u32 {
        let view = CameraView::new(&camera, 4000, height).unwrap();
        assert_eq!(
            view.viewport.height, height,
            "window 4000x{height} produced viewport {:?}",
            view.viewport
        );
        assert_eq!(view.viewport.y, 0);
    }
}

#[test]
fn fractional_fit_fills_a_width_limited_window_exactly() {
    let camera = Camera2D {
        logical_size: [180, 320],
        ..Default::default()
    };
    for width in 1..180u32 {
        let view = CameraView::new(&camera, width, 4000).unwrap();
        assert_eq!(
            view.viewport.width, width,
            "window {width}x4000 produced viewport {:?}",
            view.viewport
        );
        assert_eq!(view.viewport.x, 0);
    }
}
