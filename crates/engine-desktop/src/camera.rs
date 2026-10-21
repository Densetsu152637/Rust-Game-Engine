use anyhow::{Result, ensure};
use engine_project::Camera2D;

/// Physical framebuffer rectangle, with a top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct CameraView {
    pub viewport: PixelRect,
    pub world: [f32; 16],
    pub ui: [f32; 16],
    pub scale: f64,
}

impl CameraView {
    /// Integer fit above 1x, fractional fit for smaller windows. World Y points
    /// up; UI Y points down. Camera centers snap in source-pixel space.
    pub fn new(camera: &Camera2D, width: u32, height: u32) -> Result<Self> {
        let [lw, lh] = camera.logical_size;
        ensure!(lw > 0 && lh > 0, "camera logical size must be positive");
        ensure!(
            camera.pixels_per_unit.is_finite() && camera.pixels_per_unit > 0.0,
            "camera pixels per unit must be finite and positive"
        );
        ensure!(
            camera.center.iter().all(|x| x.is_finite()),
            "invalid camera center"
        );
        let fit = (width as f64 / lw as f64).min(height as f64 / lh as f64);
        let scale = if fit >= 1.0 { fit.floor() } else { fit };
        let vw = ((lw as f64 * scale).floor() as u32).max(1).min(width);
        let vh = ((lh as f64 * scale).floor() as u32).max(1).min(height);
        let viewport = PixelRect {
            x: (width - vw) / 2,
            y: (height - vh) / 2,
            width: vw,
            height: vh,
        };
        let ppu = camera.pixels_per_unit;
        let mut center = camera.center;
        if camera.pixel_snap {
            center = center.map(|x| (x * ppu).round() / ppu);
        }
        let sx = 2.0 * ppu / lw as f32;
        let sy = 2.0 * ppu / lh as f32;
        ensure!(
            [sx, sy, center[0] * sx, center[1] * sy]
                .iter()
                .all(|n| n.is_finite()),
            "camera matrix exceeds finite range"
        );
        Ok(Self {
            viewport,
            scale,
            world: [
                sx,
                0.0,
                0.0,
                0.0,
                0.0,
                sy,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                -center[0] * sx,
                -center[1] * sy,
                0.0,
                1.0,
            ],
            ui: [
                2.0 / lw as f32,
                0.0,
                0.0,
                0.0,
                0.0,
                -2.0 / lh as f32,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                -1.0,
                1.0,
                0.0,
                1.0,
            ],
        })
    }

    /// Convert a logical UI clip to an intersected physical scissor. An empty
    /// intersection returns `None` and must skip the draw, not disable clipping.
    pub fn scissor(&self, clip: Option<[f32; 4]>) -> Result<Option<PixelRect>> {
        let v = self.viewport;
        if v.width == 0 || v.height == 0 {
            return Ok(None);
        }
        let Some([x, y, w, h]) = clip else {
            return Ok(Some(v));
        };
        ensure!(
            [x, y, w, h].iter().all(|n| n.is_finite()) && w >= 0.0 && h >= 0.0,
            "invalid draw clip"
        );
        if w == 0.0 || h == 0.0 {
            return Ok(None);
        }
        let left = (x as f64 * self.scale).floor().clamp(0.0, v.width as f64) as u32;
        let top = (y as f64 * self.scale).floor().clamp(0.0, v.height as f64) as u32;
        let right = ((x as f64 + w as f64) * self.scale)
            .ceil()
            .clamp(0.0, v.width as f64) as u32;
        let bottom = ((y as f64 + h as f64) * self.scale)
            .ceil()
            .clamp(0.0, v.height as f64) as u32;
        if right <= left || bottom <= top {
            return Ok(None);
        }
        Ok(Some(PixelRect {
            x: v.x + left,
            y: v.y + top,
            width: right - left,
            height: bottom - top,
        }))
    }

    pub fn logical_pointer(&self, physical: [f32; 2]) -> [f32; 2] {
        if self.scale == 0.0 {
            return [0.0; 2];
        }
        [
            ((physical[0] as f64 - self.viewport.x as f64) / self.scale) as f32,
            ((physical[1] as f64 - self.viewport.y as f64) / self.scale) as f32,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_snap_clip_and_minimize() {
        let camera = Camera2D {
            center: [0.04, -0.04],
            ..Default::default()
        };
        let v = CameraView::new(&camera, 1000, 700).unwrap();
        assert_eq!(
            v.viewport,
            PixelRect {
                x: 20,
                y: 80,
                width: 960,
                height: 540
            }
        );
        assert_eq!(v.logical_pointer([20.0, 80.0]), [0.0, 0.0]);
        assert_eq!(
            v.scissor(Some([-10.0, -10.0, 20.0, 20.0])).unwrap(),
            Some(PixelRect {
                x: 20,
                y: 80,
                width: 30,
                height: 30
            })
        );
        assert_eq!(v.scissor(Some([400.0, 0.0, 1.0, 1.0])).unwrap(), None);
        assert_eq!(v.scissor(Some([0.1, 0.1, 0.0, 1.0])).unwrap(), None);
        assert!(v.world[12] < 0.0 && v.world[13] > 0.0);
        assert_eq!(
            CameraView::new(&camera, 0, 0)
                .unwrap()
                .scissor(None)
                .unwrap(),
            None
        );
        assert_eq!(CameraView::new(&camera, 160, 90).unwrap().scale, 0.5);
    }
}
