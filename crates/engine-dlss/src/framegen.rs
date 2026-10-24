//! DLSS Frame Generation (DLSS-G) contract. These types compile in every
//! build; native evaluation exists only with the official SDK (`ngx_available`).
//! Generating a frame is not presenting it: the caller owns pacing and
//! presentation order (see docs/dlss.md).
use crate::{Error, Result};

#[cfg(ngx_available)]
mod native;
#[cfg(ngx_available)]
pub use native::FrameGenerator;

/// Backbuffer/HUD-less/output color format. NGX receives the matching VkFormat
/// as the native backbuffer format; the generated frame uses the same format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorFormat {
    /// SDR swapchain-style color.
    Rgba8Unorm,
    /// Linear/HDR color; set [`GenerationFrame::hdr`] when presenting HDR.
    Rgba16Float,
}

/// Creation-time configuration. Changing any field requires a new generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameGenerationOptions {
    /// Backbuffer, HUD-less and generated-frame size.
    pub output_size: [u32; 2],
    /// Depth and motion-vector size (the render resolution before upscaling).
    pub render_size: [u32; 2],
    pub color_format: ColorFormat,
    /// Whether every evaluation supplies a HUD-less scene color. Required for
    /// FG to keep UI stable; without it the backbuffer must contain no UI.
    pub hudless: bool,
}
impl FrameGenerationOptions {
    pub fn validate(&self, maximum_dimension: u32) -> Result<()> {
        let valid = |size: [u32; 2]| size.iter().all(|&x| x > 0 && x <= maximum_dimension);
        if !valid(self.output_size) || !valid(self.render_size) {
            return Err(Error::InvalidInput(format!(
                "frame generation sizes must be nonzero and at most {maximum_dimension}"
            )));
        }
        if self.render_size[0] > self.output_size[0] || self.render_size[1] > self.output_size[1] {
            return Err(Error::InvalidInput(
                "frame generation render size cannot exceed the output size".into(),
            ));
        }
        Ok(())
    }
}

/// Unjittered camera constants for the current real frame. Matrices are passed
/// verbatim as NVIDIA `float[4][4]` in row-major order assuming
/// post-multiplication (`clip = M * v` with column vectors; DLSS-FG guide
/// 310.7.0, "Required Input Engine Data"). They must agree with the depth
/// buffer and motion vectors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub view_to_clip: [[f32; 4]; 4],
    pub clip_to_view: [[f32; 4]; 4],
    /// Lens distortion in clip space; identity when no distortion is applied.
    pub clip_to_lens_clip: [[f32; 4]; 4],
    pub clip_to_prev_clip: [[f32; 4]; 4],
    pub prev_clip_to_clip: [[f32; 4]; 4],
    pub position: [f32; 3],
    pub up: [f32; 3],
    pub right: [f32; 3],
    pub forward: [f32; 3],
    pub near: f32,
    pub far: f32,
    /// Vertical field of view in radians (ignored for orthographic cameras).
    pub fov_y: f32,
    /// View-space width divided by height.
    pub aspect: f32,
    pub pinhole_offset: [f32; 2],
}
const IDENTITY: [[f32; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
impl Default for Camera {
    /// A static camera at the origin; callers must supply real matrices.
    fn default() -> Self {
        Self {
            view_to_clip: IDENTITY,
            clip_to_view: IDENTITY,
            clip_to_lens_clip: IDENTITY,
            clip_to_prev_clip: IDENTITY,
            prev_clip_to_clip: IDENTITY,
            position: [0.; 3],
            up: [0., 1., 0.],
            right: [1., 0., 0.],
            forward: [0., 0., 1.],
            near: 0.1,
            far: 1000.,
            fov_y: std::f32::consts::FRAC_PI_2,
            aspect: 1.,
            pinhole_offset: [0.; 2],
        }
    }
}

/// Which generated frame of the current real-frame pair to produce.
/// `count` 1 is 2x; multi-frame generation asks for indices `1..=count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interpolation {
    pub index: u32,
    pub count: u32,
}
impl Default for Interpolation {
    fn default() -> Self {
        Self { index: 1, count: 1 }
    }
}

/// Per-real-frame FG constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GenerationFrame {
    pub camera: Camera,
    /// Projection jitter of the current frame (NGX "JitterOffset", documented
    /// only as a clip-space offset); zero without TAA/DLSS SR jitter. Matrices
    /// and motion vectors must exclude jitter.
    pub jitter: [f32; 2],
    /// Multiply stored motion by this value to obtain current-to-previous
    /// screen-space motion in pixels of the motion texture (NGX "MvecScale"):
    /// 1 for pixel motion, `render_size` for UV motion, negative to flip an axis.
    pub motion_scale: [f32; 2],
    /// History is unrelated to the previous real frame: camera cut, teleport,
    /// resize, first frame after FG was paused, or a scene/load transition.
    pub reset: bool,
    pub depth_inverted: bool,
    /// Motion vectors already include camera motion (not only object motion).
    pub camera_motion_included: bool,
    /// Color buffers hold HDR values for an HDR display.
    pub hdr: bool,
    pub orthographic: bool,
    pub interpolation: Interpolation,
}
impl Default for GenerationFrame {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            jitter: [0.; 2],
            motion_scale: [1.; 2],
            reset: true,
            depth_inverted: false,
            camera_motion_included: true,
            hdr: false,
            orthographic: false,
            interpolation: Interpolation::default(),
        }
    }
}
impl GenerationFrame {
    pub fn validate(&self) -> Result<()> {
        let c = &self.camera;
        let finite = [
            c.view_to_clip,
            c.clip_to_view,
            c.clip_to_lens_clip,
            c.clip_to_prev_clip,
            c.prev_clip_to_clip,
        ]
        .iter()
        .flatten()
        .flatten()
        .chain(
            c.position
                .iter()
                .chain(&c.up)
                .chain(&c.right)
                .chain(&c.forward),
        )
        .chain(&c.pinhole_offset)
        .chain(&self.jitter)
        .chain(&self.motion_scale)
        .chain(&[c.near, c.far, c.fov_y, c.aspect])
        .all(|x| x.is_finite());
        if !finite {
            return Err(Error::InvalidInput(
                "frame generation constants must be finite".into(),
            ));
        }
        if self.motion_scale.contains(&0.) {
            return Err(Error::InvalidInput(
                "frame generation motion scale must be nonzero".into(),
            ));
        }
        if c.near <= 0. || c.far <= c.near || c.aspect <= 0. {
            return Err(Error::InvalidInput(
                "frame generation camera needs 0 < near < far and positive aspect".into(),
            ));
        }
        if !self.orthographic && !(c.fov_y > 0. && c.fov_y < std::f32::consts::PI) {
            return Err(Error::InvalidInput(
                "perspective frame generation needs a vertical FOV in (0, pi)".into(),
            ));
        }
        let Interpolation { index, count } = self.interpolation;
        if count == 0 || index == 0 || index > count {
            return Err(Error::InvalidInput(format!(
                "interpolation index {index} must be in 1..={count} with a nonzero count"
            )));
        }
        Ok(())
    }
}

/// Outcome of one successful generation. Only `Presentable` frames belong
/// between the previous and current real frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generated {
    /// Interpolated between the previous and current real frames.
    Presentable,
    /// Reset or first frame: there is no valid previous frame. Present only the
    /// real frame.
    NoHistory,
    /// NGX's OutputDisableInterpolation hint: interpolation was disabled and the
    /// output is a copy of the backbuffer, so the generated frame is skipped.
    Disabled,
}

#[derive(Clone, Debug)]
pub struct FrameGenerationStatus {
    pub device_name: String,
    pub driver_version: u32,
    /// NGX-reported minimum driver for FG (`FrameGeneration.MinDriverVersion*`).
    pub minimum_driver: [u32; 2],
    /// Maximum generated frames per real-frame pair (1 means no multi-frame).
    pub multi_frame_max: u32,
    pub sdk_api_version: u32,
    pub output_size: [u32; 2],
    pub render_size: [u32; 2],
    pub color_format: ColorFormat,
    /// Successful native evaluations (including NoHistory/Disabled outcomes).
    pub evaluated_frames: u64,
}

/// Caller textures for one real frame. Each must be a single-mip 2D texture
/// with `COPY_SRC` usage: backbuffer/HUD-less in the configured color format at
/// output size, `R32Float` depth and `Rg16Float` motion at render size.
#[cfg(feature = "ngx-vulkan")]
#[derive(Clone, Copy, Debug)]
pub struct GenerationInputs<'a> {
    /// Final color of the current real frame (with UI if `hudless` is set).
    pub backbuffer: &'a wgpu::Texture,
    /// Scene color before UI; required exactly when options.hudless is true.
    pub hudless: Option<&'a wgpu::Texture>,
    pub depth: &'a wgpu::Texture,
    pub motion: &'a wgpu::Texture,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> FrameGenerationOptions {
        FrameGenerationOptions {
            output_size: [1920, 1080],
            render_size: [1280, 720],
            color_format: ColorFormat::Rgba8Unorm,
            hudless: true,
        }
    }

    #[test]
    fn options_reject_zero_oversized_and_inverted_sizes() {
        assert!(options().validate(8192).is_ok());
        for bad in [
            FrameGenerationOptions {
                output_size: [0, 1080],
                ..options()
            },
            FrameGenerationOptions {
                render_size: [1280, 0],
                ..options()
            },
            FrameGenerationOptions {
                output_size: [9000, 1080],
                ..options()
            },
            FrameGenerationOptions {
                render_size: [2560, 720],
                ..options()
            },
        ] {
            assert!(matches!(bad.validate(8192), Err(Error::InvalidInput(_))));
        }
    }

    #[test]
    fn frame_rejects_invalid_camera_motion_and_interpolation() {
        assert!(GenerationFrame::default().validate().is_ok());
        let mut nan = GenerationFrame::default();
        nan.camera.clip_to_prev_clip[2][3] = f32::NAN;
        let mut infinite_far = GenerationFrame::default();
        infinite_far.camera.far = f32::INFINITY;
        let mut inverted = GenerationFrame::default();
        inverted.camera.far = inverted.camera.near;
        let mut fov = GenerationFrame::default();
        fov.camera.fov_y = std::f32::consts::PI;
        let mut aspect = GenerationFrame::default();
        aspect.camera.aspect = 0.;
        let bad = [
            nan,
            infinite_far,
            inverted,
            fov,
            aspect,
            GenerationFrame {
                motion_scale: [1., 0.],
                ..Default::default()
            },
            GenerationFrame {
                jitter: [f32::NAN, 0.],
                ..Default::default()
            },
            GenerationFrame {
                interpolation: Interpolation { index: 0, count: 1 },
                ..Default::default()
            },
            GenerationFrame {
                interpolation: Interpolation { index: 3, count: 2 },
                ..Default::default()
            },
            GenerationFrame {
                interpolation: Interpolation { index: 1, count: 0 },
                ..Default::default()
            },
        ];
        for frame in bad {
            assert!(
                matches!(frame.validate(), Err(Error::InvalidInput(_))),
                "{frame:?}"
            );
        }
    }

    #[test]
    fn orthographic_frames_do_not_require_a_field_of_view() {
        let mut frame = GenerationFrame {
            orthographic: true,
            ..Default::default()
        };
        frame.camera.fov_y = 0.;
        assert!(frame.validate().is_ok());
    }
}
