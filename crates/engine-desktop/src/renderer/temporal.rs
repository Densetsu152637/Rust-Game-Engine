//! Temporal inputs shared by upscalers: sub-pixel jitter, previous-frame
//! matrices and current-to-previous motion. Pure CPU math, tested without a GPU.

#![cfg_attr(not(feature = "dlss"), allow(dead_code))]

use engine_project::rendering3d::{CameraInstance3D, MeshDraw3D, Projection3D};
use glam::{Mat4, Vec3, Vec4};
use std::collections::HashMap;

/// The engine depth/motion shader (`vs_main`/`fs_main`, one uniform at 0/0).
pub const MOTION_WGSL: &str = include_str!("motion.wgsl");

/// Radical inverse of `index` (>= 1) in `base`: the Halton sequence in (0, 1).
pub fn halton(mut index: u32, base: u32) -> f32 {
    let mut fraction = 1.0;
    let mut result = 0.0;
    while index > 0 {
        fraction /= base as f32;
        result += fraction * (index % base) as f32;
        index /= base;
    }
    result
}

/// Jitter phases for a render/output ratio: NVIDIA's DLSS guidance of eight
/// phases times the squared per-axis scale, clamped to 8..=256.
pub fn jitter_phase_count(render: [u32; 2], output: [u32; 2]) -> u32 {
    let scale = output[0] as f32 / render[0].max(1) as f32;
    ((8.0 * scale * scale).ceil() as u32).clamp(8, 256)
}

/// Halton(2,3) sub-pixel jitter in render pixels, each axis in (-0.5, 0.5),
/// for 0-based `frame` within `phases`.
pub fn jitter_offset(frame: u64, phases: u32) -> [f32; 2] {
    let index = (frame % u64::from(phases.max(1))) as u32 + 1;
    [halton(index, 2) - 0.5, halton(index, 3) - 0.5]
}

/// Clip-space translation that moves rasterization by `jitter` render pixels
/// (pixel x right, y down). Premultiply onto a view-projection matrix; it
/// applies equally to perspective and orthographic projections.
pub fn jitter_matrix(jitter: [f32; 2], render: [u32; 2]) -> Mat4 {
    Mat4::from_translation(Vec3::new(
        2.0 * jitter[0] / render[0] as f32,
        -2.0 * jitter[1] / render[1] as f32,
        0.0,
    ))
}

/// Current-to-previous motion in render pixels (y down) of one point, the CPU
/// reference of `motion.wgsl`. Points behind either camera have no motion.
pub fn pixel_motion(current: Vec4, previous: Vec4, render: [u32; 2]) -> [f32; 2] {
    if current.w <= 1e-6 || previous.w <= 1e-6 {
        return [0.0; 2];
    }
    let delta =
        previous.truncate().truncate() / previous.w - current.truncate().truncate() / current.w;
    [
        delta.x * 0.5 * render[0] as f32,
        -delta.y * 0.5 * render[1] as f32,
    ]
}

/// Uniform block of `motion.wgsl` for one draw.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MotionUniform {
    pub jittered: [[f32; 4]; 4],
    pub current: [[f32; 4]; 4],
    pub previous: [[f32; 4]; 4],
    pub size: [f32; 4],
}

/// Matrices one temporal frame needs for every draw.
pub(crate) struct TemporalFrame<'a> {
    pub jitter: Mat4,
    pub current_vp: Mat4,
    pub previous_vp: Mat4,
    pub previous_models: &'a HashMap<String, Mat4>,
    pub render_size: [u32; 2],
}
impl TemporalFrame<'_> {
    /// Motion uniform of `draw`. A draw without history (new this frame, or
    /// after a reset) uses its current model, so only camera motion remains.
    pub fn uniform(&self, draw: &MeshDraw3D) -> MotionUniform {
        let model = Mat4::from_cols_array_2d(&draw.model);
        let previous_model = self.previous_models.get(&draw.id).copied().unwrap_or(model);
        MotionUniform {
            jittered: (self.jitter * self.current_vp * model).to_cols_array_2d(),
            current: (self.current_vp * model).to_cols_array_2d(),
            previous: (self.previous_vp * previous_model).to_cols_array_2d(),
            size: [
                self.render_size[0] as f32,
                self.render_size[1] as f32,
                0.0,
                0.0,
            ],
        }
    }
}

/// Previous-frame camera and object matrices plus the jitter phase.
#[derive(Default)]
pub(crate) struct TemporalHistory {
    previous_vp: Option<Mat4>,
    projection: Option<Projection3D>,
    models: HashMap<String, Mat4>,
    frame: u64,
    reset_requested: bool,
}
impl TemporalHistory {
    /// Discard history before the next temporal frame (camera cut, scene change).
    pub fn request_reset(&mut self) {
        self.reset_requested = true;
    }

    /// Whether the next frame must reset: an explicit request, no previous
    /// frame, or a changed projection (FOV/near/far/kind) are history breaks.
    pub fn needs_reset(&self, camera: &CameraInstance3D) -> bool {
        self.reset_requested
            || self.previous_vp.is_none()
            || self.projection.as_ref() != Some(&camera.camera.projection)
    }

    /// Previous view-projection, or `current` when history is reset.
    pub fn previous_vp(&self, current: Mat4, reset: bool) -> Mat4 {
        if reset {
            current
        } else {
            self.previous_vp.unwrap_or(current)
        }
    }

    pub fn previous_models(&self) -> &HashMap<String, Mat4> {
        &self.models
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Record the frame just evaluated as the next frame's history.
    pub fn commit(&mut self, camera: &CameraInstance3D, current_vp: Mat4, meshes: &[MeshDraw3D]) {
        self.previous_vp = Some(current_vp);
        self.projection = Some(camera.camera.projection.clone());
        self.models.clear();
        self.models.extend(
            meshes
                .iter()
                .map(|draw| (draw.id.clone(), Mat4::from_cols_array_2d(&draw.model))),
        );
        self.frame = self.frame.wrapping_add(1);
        self.reset_requested = false;
    }

    /// Forget everything; the next temporal frame resets.
    pub fn clear(&mut self) {
        *self = Self {
            reset_requested: true,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::rendering3d::Camera3D;

    #[test]
    fn halton_matches_the_radical_inverse() {
        let base2: Vec<f32> = (1..=4).map(|i| halton(i, 2)).collect();
        assert_eq!(base2, [0.5, 0.25, 0.75, 0.125]);
        let base3: Vec<f32> = (1..=4).map(|i| halton(i, 3)).collect();
        for (actual, expected) in base3
            .iter()
            .zip([1.0 / 3.0, 2.0 / 3.0, 1.0 / 9.0, 4.0 / 9.0])
        {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn jitter_is_subpixel_zero_mean_and_repeats_after_its_phase_count() {
        let phases = 8;
        let samples: Vec<_> = (0..phases)
            .map(|f| jitter_offset(f, phases as u32))
            .collect();
        assert!(samples.iter().flatten().all(|v| v.abs() < 0.5));
        let mean: [f32; 2] = [0, 1].map(|axis| samples.iter().map(|s| s[axis]).sum::<f32>() / 8.0);
        assert!(mean.iter().all(|m| m.abs() < 0.07), "{mean:?}");
        assert_eq!(jitter_offset(0, 8), jitter_offset(8, 8));
        assert_ne!(jitter_offset(0, 8), jitter_offset(1, 8));
        assert_eq!(jitter_offset(0, 8), [0.0, 1.0 / 3.0 - 0.5]);
    }

    #[test]
    fn phase_count_grows_with_the_upscale_ratio() {
        assert_eq!(jitter_phase_count([1920, 1080], [1920, 1080]), 8);
        assert_eq!(jitter_phase_count([1280, 720], [1920, 1080]), 18);
        assert_eq!(jitter_phase_count([960, 540], [1920, 1080]), 32);
        assert_eq!(jitter_phase_count([1, 1], [4096, 4096]), 256);
    }

    #[test]
    fn jitter_matrix_moves_pixels_right_and_down_by_the_offset() {
        let render = [200, 100];
        let jitter = jitter_matrix([0.25, 0.5], render);
        let projected = jitter * Vec4::new(0.0, 0.0, 0.5, 2.0);
        let ndc = projected.truncate() / projected.w;
        // NDC to pixel: x = (ndc.x+1)/2*w, y = (1-ndc.y)/2*h.
        let pixel = [(ndc.x + 1.0) * 0.5 * 200.0, (1.0 - ndc.y) * 0.5 * 100.0];
        assert!((pixel[0] - 100.25).abs() < 1e-4, "{pixel:?}");
        assert!((pixel[1] - 50.5).abs() < 1e-4, "{pixel:?}");
        assert!((ndc.z - 0.25).abs() < 1e-6, "jitter never changes depth");
    }

    #[test]
    fn motion_points_from_the_current_pixel_to_the_previous_pixel() {
        let render = [100, 100];
        let vp = Mat4::IDENTITY;
        // A point that moved right by 0.2 NDC (10 px) and up by 0.1 NDC (5 px up = -5 y).
        let previous = vp * Vec4::new(-0.2, -0.1, 0.5, 1.0);
        let current = vp * Vec4::new(0.0, 0.0, 0.5, 1.0);
        let motion = pixel_motion(current, previous, render);
        assert!((motion[0] + 10.0).abs() < 1e-4, "{motion:?}");
        assert!((motion[1] - 5.0).abs() < 1e-4, "{motion:?}");
        assert_eq!(pixel_motion(current, current, render), [0.0, 0.0]);
        assert_eq!(
            pixel_motion(current, Vec4::new(0.0, 0.0, 0.0, -1.0), render),
            [0.0; 2]
        );
    }

    #[test]
    fn motion_combines_camera_and_object_motion_and_excludes_jitter() {
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let projection = crate::camera_matrix(&camera, 1.0).unwrap();
        let mut moved = camera.clone();
        moved.world = Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0)).to_cols_array_2d();
        let current_vp = crate::camera_matrix(&moved, 1.0).unwrap();
        let draw = MeshDraw3D {
            id: "box".into(),
            mesh: Default::default(),
            texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            model: Mat4::from_translation(Vec3::new(0.0, 0.0, -5.0)).to_cols_array_2d(),
            material: Default::default(),
            shader: None,
        };
        let previous_models: HashMap<_, _> = [(
            "box".to_owned(),
            Mat4::from_translation(Vec3::new(-0.25, 0.0, -5.0)),
        )]
        .into();
        let render = [64, 64];
        let frame = TemporalFrame {
            jitter: jitter_matrix([0.4, -0.3], render),
            current_vp,
            previous_vp: projection,
            previous_models: &previous_models,
            render_size: render,
        };
        let uniform = frame.uniform(&draw);
        let origin = Vec4::new(0.0, 0.0, 0.0, 1.0);
        let current = Mat4::from_cols_array_2d(&uniform.current) * origin;
        let previous = Mat4::from_cols_array_2d(&uniform.previous) * origin;
        let motion = pixel_motion(current, previous, render);
        // The object sits 0.5 left of the camera now and was 0.25 left of the
        // previous camera: on screen it appeared further right before.
        let expected = {
            let now = projection * Vec4::new(-0.5, 0.0, -5.0, 1.0);
            let before = projection * Vec4::new(-0.25, 0.0, -5.0, 1.0);
            pixel_motion(now, before, render)
        };
        assert!(
            motion[0] > 0.0 && (motion[0] - expected[0]).abs() < 1e-3,
            "{motion:?}"
        );
        assert!(motion[1].abs() < 1e-4);
        let jittered = Mat4::from_cols_array_2d(&uniform.jittered) * origin;
        let unjittered = pixel_motion(current, jittered, render);
        assert!((unjittered[0] - 0.4).abs() < 1e-3 && (unjittered[1] + 0.3).abs() < 1e-3);
        let without_history = TemporalFrame {
            previous_models: &HashMap::new(),
            previous_vp: current_vp,
            ..frame
        }
        .uniform(&draw);
        assert_eq!(without_history.previous, without_history.current);
    }

    #[test]
    fn history_resets_on_first_frame_request_and_projection_change() {
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let mut history = TemporalHistory::default();
        assert!(history.needs_reset(&camera), "no previous frame");
        history.commit(&camera, Mat4::IDENTITY, &[]);
        assert!(!history.needs_reset(&camera));
        assert_eq!(history.frame(), 1);
        let mut zoomed = camera.clone();
        zoomed.camera.projection = Projection3D::Perspective {
            fov_y_radians: 0.5,
            near: 0.1,
            far: 100.0,
        };
        assert!(history.needs_reset(&zoomed), "projection change is a cut");
        history.request_reset();
        assert!(history.needs_reset(&camera));
        assert_eq!(history.previous_vp(Mat4::ZERO, true), Mat4::ZERO);
        history.commit(&camera, Mat4::IDENTITY, &[]);
        assert!(!history.needs_reset(&camera));
        history.clear();
        assert!(history.needs_reset(&camera));
        assert_eq!(history.frame(), 0);
    }
}
