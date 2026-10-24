//! Per-view data: camera matrices (jittered and unjittered), inverses, the
//! previous frame's view-projection, sizes, frame index and time, plus the
//! GPU uniform every feature reads through `engine::view`.

use crate::feature::{FeatureNeeds, FeatureOverrides};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3, Vec4};

/// Identifies a view across frames (history, targets, status). Hosts pick
/// stable ids: the active camera is [`ViewId::MAIN`]; editor viewports, XR
/// eyes and probe/shadow views use their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(pub u64);

impl ViewId {
    pub const MAIN: ViewId = ViewId(0);
    /// Left and right XR eyes.
    pub const LEFT_EYE: ViewId = ViewId(1);
    pub const RIGHT_EYE: ViewId = ViewId(2);
}

/// What a view is for. Probe and shadow views skip post-processing and
/// overlays in later releases; today every kind renders the full stage list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewKind {
    #[default]
    Camera,
    EditorViewport,
    XrEye,
    Probe,
}

/// Rectangle in output-target pixels (top-left origin).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    pub fn full(size: [u32; 2]) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size[0],
            height: size[1],
        }
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }
}

/// CPU description of one view for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewData {
    pub id: ViewId,
    pub kind: ViewKind,
    /// World-from-view (the camera transform).
    pub world_from_view: Mat4,
    /// Unjittered view-to-clip projection (WebGPU depth 0..1, not reversed).
    pub clip_from_view: Mat4,
    /// Sub-pixel jitter in render pixels (x right, y down); zero when no
    /// temporal feature asks for it.
    pub jitter: [f32; 2],
    /// Size of the HDR scene targets (render resolution).
    pub render_size: [u32; 2],
    /// Size the upscale stage produces and the output stage reads.
    pub output_size: [u32; 2],
    /// Seconds since the host started rendering and since its last frame.
    pub time: f32,
    pub delta_time: f32,
    /// Ambient radiance (linear RGB) for the built-in lit material.
    pub ambient: Vec3,
    /// Per-view feature enable overrides.
    pub features: FeatureOverrides,
    /// Inputs the host itself needs (DLSS needs motion vectors).
    pub host_needs: FeatureNeeds,
    /// Discard history this frame (camera cut): previous matrices equal the
    /// current ones and motion is camera-only.
    pub reset_history: bool,
}

impl ViewData {
    /// A view from a camera transform and projection. Sizes must be nonzero
    /// and the matrices finite and invertible.
    pub fn new(
        id: ViewId,
        world_from_view: Mat4,
        clip_from_view: Mat4,
        render_size: [u32; 2],
        output_size: [u32; 2],
    ) -> Result<Self> {
        let view = Self {
            id,
            kind: ViewKind::Camera,
            world_from_view,
            clip_from_view,
            jitter: [0.0; 2],
            render_size,
            output_size,
            time: 0.0,
            delta_time: 0.0,
            ambient: Vec3::ZERO,
            features: FeatureOverrides::default(),
            host_needs: FeatureNeeds::default(),
            reset_history: false,
        };
        view.validate()?;
        Ok(view)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.render_size
                .iter()
                .chain(&self.output_size)
                .all(|&n| n > 0),
            "view sizes must be nonzero"
        );
        ensure!(
            self.world_from_view.is_finite() && self.world_from_view.determinant().abs() > 1e-8,
            "view transform is singular"
        );
        ensure!(
            self.clip_from_view.is_finite() && self.clip_from_view.determinant().abs() > 0.0,
            "view projection is singular"
        );
        ensure!(
            self.jitter.iter().all(|j| j.is_finite() && j.abs() <= 1.0),
            "view jitter must be finite and within one render pixel"
        );
        ensure!(
            self.ambient.is_finite() && self.time.is_finite() && self.delta_time.is_finite(),
            "view ambient and time must be finite"
        );
        Ok(())
    }

    pub fn view_from_world(&self) -> Mat4 {
        self.world_from_view.inverse()
    }

    /// Clip-space translation moving rasterization by [`Self::jitter`].
    pub fn jitter_matrix(&self) -> Mat4 {
        Mat4::from_translation(Vec3::new(
            2.0 * self.jitter[0] / self.render_size[0] as f32,
            -2.0 * self.jitter[1] / self.render_size[1] as f32,
            0.0,
        ))
    }

    /// Jittered projection used for rasterization.
    pub fn jittered_clip_from_view(&self) -> Mat4 {
        self.jitter_matrix() * self.clip_from_view
    }

    /// Unjittered view-projection (motion vectors, culling).
    pub fn clip_from_world(&self) -> Mat4 {
        self.clip_from_view * self.view_from_world()
    }

    /// Jittered view-projection (rasterization).
    pub fn jittered_clip_from_world(&self) -> Mat4 {
        self.jitter_matrix() * self.clip_from_world()
    }

    pub fn camera_position(&self) -> Vec3 {
        self.world_from_view.w_axis.truncate()
    }

    /// The GPU uniform for this view; `previous` is last frame's unjittered
    /// view-projection (see [`ViewHistory`]).
    pub fn uniform(&self, previous: Mat4, frame_index: u32, light_count: u32) -> ViewUniform {
        let jittered_projection = self.jittered_clip_from_view();
        let jittered = self.jittered_clip_from_world();
        let render = [self.render_size[0] as f32, self.render_size[1] as f32];
        let output = [self.output_size[0] as f32, self.output_size[1] as f32];
        ViewUniform {
            clip_from_world: jittered.to_cols_array_2d(),
            unjittered_clip_from_world: self.clip_from_world().to_cols_array_2d(),
            view_from_world: self.view_from_world().to_cols_array_2d(),
            world_from_view: self.world_from_view.to_cols_array_2d(),
            clip_from_view: jittered_projection.to_cols_array_2d(),
            unjittered_clip_from_view: self.clip_from_view.to_cols_array_2d(),
            view_from_clip: jittered_projection.inverse().to_cols_array_2d(),
            world_from_clip: jittered.inverse().to_cols_array_2d(),
            previous_clip_from_world: previous.to_cols_array_2d(),
            camera_position: self.camera_position().extend(1.0).to_array(),
            render_size: [render[0], render[1], 1.0 / render[0], 1.0 / render[1]],
            output_size: [output[0], output[1], 1.0 / output[0], 1.0 / output[1]],
            jitter: [
                self.jitter[0],
                self.jitter[1],
                2.0 * self.jitter[0] / render[0],
                -2.0 * self.jitter[1] / render[1],
            ],
            ambient: self.ambient.extend(0.0).to_array(),
            time: [self.time, self.delta_time, 0.0, 0.0],
            counts: [frame_index, light_count, self.id.0 as u32, 0],
        }
    }
}

/// `engine::view`'s `EngineView` uniform (std140-compatible, 688 bytes).
/// Field order and meaning are a stable contract; see the WGSL module.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewUniform {
    /// Jittered view-projection: rasterize with this.
    pub clip_from_world: [[f32; 4]; 4],
    pub unjittered_clip_from_world: [[f32; 4]; 4],
    pub view_from_world: [[f32; 4]; 4],
    pub world_from_view: [[f32; 4]; 4],
    /// Jittered projection.
    pub clip_from_view: [[f32; 4]; 4],
    pub unjittered_clip_from_view: [[f32; 4]; 4],
    /// Inverse of the jittered projection.
    pub view_from_clip: [[f32; 4]; 4],
    /// Inverse of the jittered view-projection.
    pub world_from_clip: [[f32; 4]; 4],
    /// Previous frame's unjittered view-projection (equals the current one
    /// after a history reset).
    pub previous_clip_from_world: [[f32; 4]; 4],
    /// xyz world position, w = 1.
    pub camera_position: [f32; 4],
    /// xy render size in pixels, zw reciprocals.
    pub render_size: [f32; 4],
    /// xy output size in pixels, zw reciprocals.
    pub output_size: [f32; 4],
    /// xy jitter in render pixels (y down), zw the clip-space offset.
    pub jitter: [f32; 4],
    /// rgb ambient radiance.
    pub ambient: [f32; 4],
    /// x seconds since start, y frame delta seconds.
    pub time: [f32; 4],
    /// x frame index, y light count, z view id (low 32 bits), w reserved.
    pub counts: [u32; 4],
}

/// Byte size of [`ViewUniform`].
pub const VIEW_UNIFORM_BYTES: u64 = std::mem::size_of::<ViewUniform>() as u64;

/// Previous-frame data of one view.
#[derive(Clone, Debug, Default)]
pub struct ViewHistory {
    previous_clip_from_world: Option<Mat4>,
    frame_index: u32,
}

impl ViewHistory {
    /// Previous unjittered view-projection for a frame whose current one is
    /// `current` (equal to `current` on the first frame or after a reset).
    pub fn previous(&self, current: Mat4, reset: bool) -> Mat4 {
        match self.previous_clip_from_world {
            Some(previous) if !reset => previous,
            _ => current,
        }
    }

    pub fn frame_index(&self) -> u32 {
        self.frame_index
    }

    /// Whether a frame was committed since creation.
    pub fn has_history(&self) -> bool {
        self.previous_clip_from_world.is_some()
    }

    /// Record `current` as the next frame's previous view-projection.
    pub fn commit(&mut self, current: Mat4) {
        self.previous_clip_from_world = Some(current);
        self.frame_index = self.frame_index.wrapping_add(1);
    }
}

/// Unjittered clip position of a world point, for tests and CPU culling.
pub fn project(clip_from_world: Mat4, point: Vec3) -> Vec4 {
    clip_from_world * point.extend(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perspective() -> Mat4 {
        glam::camera::rh::proj::directx::perspective(1.0, 16.0 / 9.0, 0.1, 100.0)
    }

    #[test]
    fn uniform_matrices_equal_the_camera_matrices_and_their_inverses() {
        let world_from_view = Mat4::from_rotation_translation(
            glam::Quat::from_rotation_y(0.4),
            Vec3::new(1.0, 2.0, 3.0),
        );
        let mut view = ViewData::new(
            ViewId::MAIN,
            world_from_view,
            perspective(),
            [320, 180],
            [640, 360],
        )
        .unwrap();
        let close = |a: [[f32; 4]; 4], b: Mat4| Mat4::from_cols_array_2d(&a).abs_diff_eq(b, 1e-5);
        let uniform = view.uniform(Mat4::IDENTITY, 7, 2);
        let clip_from_world = perspective() * world_from_view.inverse();
        assert!(close(uniform.clip_from_world, clip_from_world));
        assert!(close(uniform.unjittered_clip_from_world, clip_from_world));
        assert!(close(uniform.view_from_world, world_from_view.inverse()));
        assert!(close(uniform.world_from_view, world_from_view));
        assert!(close(uniform.clip_from_view, perspective()));
        assert!(close(uniform.view_from_clip, perspective().inverse()));
        assert!(close(uniform.world_from_clip, clip_from_world.inverse()));
        assert_eq!(uniform.camera_position, [1.0, 2.0, 3.0, 1.0]);
        assert_eq!(
            uniform.render_size,
            [320.0, 180.0, 1.0 / 320.0, 1.0 / 180.0]
        );
        assert_eq!(uniform.output_size[..2], [640.0, 360.0]);
        assert_eq!(uniform.counts, [7, 2, 0, 0]);

        view.jitter = [0.25, -0.5];
        let jittered = view.uniform(Mat4::IDENTITY, 0, 0);
        assert!(close(jittered.unjittered_clip_from_world, clip_from_world));
        assert!(!close(jittered.clip_from_world, clip_from_world));
        // Jitter moves a projected point by exactly the requested pixels.
        let point = Vec3::new(0.0, 0.0, -10.0);
        let pixel = |m: [[f32; 4]; 4]| {
            let p =
                Mat4::from_cols_array_2d(&m) * world_from_view.transform_point3(point).extend(1.0);
            let ndc = p.truncate() / p.w;
            [(ndc.x + 1.0) * 160.0, (1.0 - ndc.y) * 90.0]
        };
        let (a, b) = (
            pixel(jittered.clip_from_world),
            pixel(jittered.unjittered_clip_from_world),
        );
        assert!((a[0] - b[0] - 0.25).abs() < 1e-3 && (a[1] - b[1] + 0.5).abs() < 1e-3);
        assert_eq!(std::mem::size_of::<ViewUniform>(), 688);
    }

    #[test]
    fn previous_view_projection_follows_committed_frames_and_resets() {
        let mut history = ViewHistory::default();
        let first = Mat4::from_translation(Vec3::X);
        let second = Mat4::from_translation(Vec3::Y);
        assert_eq!(history.previous(first, false), first, "no history yet");
        history.commit(first);
        assert_eq!(history.previous(second, false), first);
        assert_eq!(history.previous(second, true), second, "reset");
        history.commit(second);
        assert_eq!(history.previous(Mat4::IDENTITY, false), second);
        assert_eq!(history.frame_index(), 2);
    }

    #[test]
    fn invalid_views_are_rejected() {
        let ok = |w, p, r, o| ViewData::new(ViewId::MAIN, w, p, r, o).is_ok();
        assert!(ok(Mat4::IDENTITY, perspective(), [1, 1], [1, 1]));
        assert!(!ok(Mat4::IDENTITY, perspective(), [0, 1], [1, 1]));
        assert!(!ok(Mat4::ZERO, perspective(), [1, 1], [1, 1]));
        assert!(!ok(Mat4::IDENTITY, Mat4::ZERO, [1, 1], [1, 1]));
        let mut view =
            ViewData::new(ViewId::MAIN, Mat4::IDENTITY, perspective(), [4, 4], [4, 4]).unwrap();
        view.jitter = [2.0, 0.0];
        assert!(view.validate().is_err());
    }
}
