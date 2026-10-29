//! DLSS Frame Generation stage of [`super::GpuRenderer`]: state, status and the
//! pure helpers (pacing, reset rules, NGX camera constants). The real frame is
//! rendered into an engine-owned backbuffer (with UI) and HUD-less color
//! (without UI); NGX generates the frames between the previous and current
//! real frames, which hosts present in order before the real frame.

#![cfg_attr(not(feature = "dlss"), allow(dead_code))]

use super::temporal::TemporalHistory;
use crate::dlss::UpscalerDevice;
use engine_project::rendering3d::{
    CameraInstance3D, FrameGenerationSettings, FrameGenerationState, FrameGenerationStatus,
    bounded_upscaler_reason,
};
use std::time::{Duration, Instant};

/// A real-frame gap longer than this means no game frames were rendered
/// (pause, menu, minimized window): generation restarts from a reset.
pub const MAX_GENERATION_GAP: Duration = Duration::from_millis(250);
/// Longest wait between two paced presents.
pub const MAX_PRESENT_SPACING: Duration = Duration::from_millis(50);

/// Present offsets from the start of a real frame's sequence: `generated`
/// generated frames then the real frame, evenly spaced over `interval` (the
/// recent real-frame interval). The first generated frame is presented at once.
pub fn presentation_offsets(interval: Duration, generated: usize) -> Vec<Duration> {
    let slots = generated as u32 + 1;
    let spacing = (interval / slots).min(MAX_PRESENT_SPACING);
    (0..slots).map(|i| spacing * i).collect()
}

/// Exponential average of real-frame intervals for [`presentation_offsets`].
#[derive(Default)]
pub struct PresentPacer {
    last: Option<Instant>,
    interval: Option<Duration>,
}
impl PresentPacer {
    /// Record a real frame's sequence start; returns the interval to pace with
    /// (zero before two frames, so the first sequence is not delayed).
    pub fn begin(&mut self, now: Instant) -> Duration {
        if let Some(last) = self.last {
            let measured = now.duration_since(last).min(MAX_GENERATION_GAP);
            self.interval = Some(match self.interval {
                Some(previous) => (previous * 3 + measured) / 4,
                None => measured,
            });
        }
        self.last = Some(now);
        self.interval.unwrap_or_default()
    }
    /// Forget timing after a pause so a stale interval is not reused.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Request key a sticky failure applies to (retried after it changes).
type FailureKey = (FrameGenerationSettings, [u32; 2], [u32; 2]);

pub(crate) struct FrameGeneration {
    pub(super) history: TemporalHistory,
    status: FrameGenerationStatus,
    pub(super) generated: u64,
    pub(super) presented: u64,
    pub(super) resets: u64,
    failure: Option<(FailureKey, String)>,
    /// When the last generated sequence finished; a long gap resets history.
    last_frame: Option<Instant>,
    last_completed_source: Option<(u64, bool)>,
    /// Indices of presentable generated textures of the last real frame, in
    /// presentation order.
    pub(super) presentable: Vec<usize>,
    /// The engine backbuffer holds a real frame ready to present.
    pub(super) real_ready: bool,
    /// 3D viewport of the last real frame in target pixels.
    pub(super) viewport: [u32; 4],
    #[cfg(feature = "dlss")]
    pub(super) stage: Option<stage::FgStage>,
}

impl FrameGeneration {
    pub fn new() -> Self {
        Self {
            history: TemporalHistory::default(),
            status: FrameGenerationStatus::default(),
            generated: 0,
            presented: 0,
            resets: 0,
            failure: None,
            last_frame: None,
            last_completed_source: None,
            presentable: Vec::new(),
            real_ready: false,
            viewport: [0; 4],
            #[cfg(feature = "dlss")]
            stage: None,
        }
    }

    pub fn status(&self) -> &FrameGenerationStatus {
        &self.status
    }

    /// Release the generator and history (new device or explicit restart).
    pub fn clear(&mut self) {
        #[cfg(feature = "dlss")]
        {
            self.stage = None;
        }
        self.history.clear();
        self.last_completed_source = None;
        self.failure = None;
        self.discard_frames();
    }

    pub fn request_reset(&mut self) {
        self.history.request_reset();
    }

    /// Generated and real frames of the previous real frame are stale.
    pub fn discard_frames(&mut self) {
        self.presentable.clear();
        self.real_ready = false;
    }

    /// History breaks for frame generation: SR's triggers (first frame,
    /// explicit reset, projection change) plus a gap without game frames.
    pub fn needs_reset(&self, camera: &CameraInstance3D, now: Instant) -> bool {
        self.history.needs_reset(camera)
            || self
                .last_frame
                .is_none_or(|last| now.duration_since(last) > MAX_GENERATION_GAP)
    }

    pub fn finish_frame(&mut self, now: Instant) {
        self.last_frame = Some(now);
    }

    /// Finish the already-rendered source once, even when interpolation fails.
    /// The caller retains its valid real colour and must not rerender the scene.
    pub fn complete_evaluation(
        &mut self,
        source_id: u64,
        camera: &CameraInstance3D,
        now: Instant,
        request: FrameGenerationSettings,
        sizes: ([u32; 2], [u32; 2]),
        failure: Option<&str>,
    ) -> bool {
        if let Some((completed, success)) = self.last_completed_source
            && completed == source_id
        {
            return success;
        }
        let success = failure.is_none();
        self.last_completed_source = Some((source_id, success));
        self.history.commit(camera);
        self.finish_frame(now);
        if let Some(reason) = failure {
            self.remember_failure(request, sizes, reason);
            self.fallback(request, reason);
            false
        } else {
            true
        }
    }

    /// Not requested: only real frames, and history restarts when it is.
    pub fn off(&mut self, requested: FrameGenerationSettings) {
        self.history.request_reset();
        self.discard_frames();
        self.status = self.counted(requested, FrameGenerationState::Off);
    }

    /// Requested but not running for this frame.
    pub fn fallback(&mut self, requested: FrameGenerationSettings, reason: &str) {
        self.history.request_reset();
        self.presentable.clear();
        self.status = FrameGenerationStatus {
            fallback_reason: Some(bounded_upscaler_reason(reason)),
            multi_frame_max: self.status.multi_frame_max,
            ..self.counted(requested, FrameGenerationState::Fallback)
        };
    }

    pub fn active(&mut self, requested: FrameGenerationSettings, multi_frame_max: u32) {
        self.status = FrameGenerationStatus {
            multiplier: Some(requested.multiplier),
            multi_frame_max: Some(multi_frame_max),
            ..self.counted(requested, FrameGenerationState::Active)
        };
    }

    pub fn note_presented(&mut self, count: usize) {
        self.presented += count as u64;
        self.status.presented_frames = self.presented;
    }

    fn counted(
        &self,
        requested: FrameGenerationSettings,
        state: FrameGenerationState,
    ) -> FrameGenerationStatus {
        FrameGenerationStatus {
            requested,
            state,
            generated_frames: self.generated,
            presented_frames: self.presented,
            history_resets: self.resets,
            ..Default::default()
        }
    }

    /// Why a request cannot run before GPU work, if known.
    pub fn precheck(
        &self,
        requested: FrameGenerationSettings,
        device: &UpscalerDevice,
        presentation_blocker: Option<&str>,
        sizes: ([u32; 2], [u32; 2]),
    ) -> Option<String> {
        if let Err(error) = requested.validate() {
            return Some(error.to_string());
        }
        if let Some(reason) = device.frame_generation_unavailable_reason() {
            return Some(reason);
        }
        if let Some(reason) = presentation_blocker {
            return Some(reason.to_owned());
        }
        self.failure
            .as_ref()
            .filter(|(key, _)| *key == (requested, sizes.0, sizes.1))
            .map(|(_, reason)| reason.clone())
    }

    pub fn remember_failure(
        &mut self,
        requested: FrameGenerationSettings,
        sizes: ([u32; 2], [u32; 2]),
        reason: &str,
    ) {
        self.failure = Some(((requested, sizes.0, sizes.1), reason.to_owned()));
    }
}

/// `m` as NVIDIA's row-major `float[4][4]` for post-multiplication
/// (`clip = M * v`, column vectors): row `r` is `m`'s row `r`.
pub fn row_major(m: glam::Mat4) -> [[f32; 4]; 4] {
    m.transpose().to_cols_array_2d()
}

/// NGX `JitterOffset` for a frame whose scene was rendered with SR jitter
/// `jitter` (render pixels, x right, y down; zero without SR). The FG guide
/// calls it a clip-space offset without a unit. Render pixels match the SR
/// input convention; on the RTX 5080 SR+FG test, pixels, clip units, zero and
/// negated jitter produced identical generated frames (see docs/dlss.md), so
/// the unit is not observable there.
pub fn generation_jitter(jitter: [f32; 2], _render: [u32; 2]) -> [f32; 2] {
    jitter
}

/// NGX camera constants for an unjittered frame. `projection` is the view to
/// clip matrix and `current`/`previous` the view-projections of this and the
/// previous real frame (equal after a reset).
#[cfg(feature = "dlss")]
pub fn generation_camera(
    camera: &CameraInstance3D,
    aspect: f32,
    projection: glam::Mat4,
    current: glam::Mat4,
    previous: glam::Mat4,
) -> engine_dlss::Camera {
    use engine_project::rendering3d::Projection3D;
    use glam::{Mat4, Vec3};
    let world = Mat4::from_cols_array_2d(&camera.world);
    let (near, far, fov_y) = match camera.camera.projection {
        Projection3D::Perspective {
            fov_y_radians,
            near,
            far,
        } => (near, far, fov_y_radians),
        Projection3D::Orthographic { near, far, .. } => (near, far, 0.0),
    };
    let clip_to_prev_clip = previous * current.inverse();
    let unit = |v: Vec3| v.normalize_or_zero().to_array();
    engine_dlss::Camera {
        view_to_clip: row_major(projection),
        clip_to_view: row_major(projection.inverse()),
        clip_to_lens_clip: row_major(Mat4::IDENTITY),
        clip_to_prev_clip: row_major(clip_to_prev_clip),
        prev_clip_to_clip: row_major(clip_to_prev_clip.inverse()),
        position: world.w_axis.truncate().to_array(),
        up: unit(world.y_axis.truncate()),
        right: unit(world.x_axis.truncate()),
        // The engine camera looks down its local negative Z axis.
        forward: unit(-world.z_axis.truncate()),
        near,
        far,
        fov_y,
        aspect,
        pinhole_offset: [0.0; 2],
    }
}

/// GPU resources and the NGX generator (SDK builds only).
#[cfg(feature = "dlss")]
pub(super) mod stage {
    /// Engine color textures are `Rgba8Unorm` (NGX's SDR format) holding
    /// sRGB-encoded bytes: rendered and sampled through an sRGB view.
    pub(crate) struct ColorTarget {
        pub texture: wgpu::Texture,
        pub view: wgpu::TextureView,
    }
    impl ColorTarget {
        pub fn new(device: &wgpu::Device, label: &str, size: [u32; 2]) -> Self {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                ..Default::default()
            });
            Self { texture, view }
        }
    }

    fn data_texture(
        device: &wgpu::Device,
        label: &str,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    /// One generator plus its fixed-size textures. Output and render size
    /// changes need a new stage (and NGX resets on its first evaluation).
    pub(crate) struct FgStage {
        pub output: [u32; 2],
        pub render: [u32; 2],
        pub generator: engine_dlss::FrameGenerator,
        /// Failed interpolation keeps real colour presentable until replacement.
        pub poisoned: bool,
        pub hudless: ColorTarget,
        pub backbuffer: ColorTarget,
        pub generated: Vec<ColorTarget>,
        /// Depth (`R32Float`) and motion (`Rg16Float`) at the render size,
        /// exported from the render pipeline each real frame.
        pub depth: (wgpu::Texture, wgpu::TextureView),
        pub motion: (wgpu::Texture, wgpu::TextureView),
    }
    impl FgStage {
        pub fn new(
            context: engine_dlss::Context,
            session: &engine_dlss::SessionOptions,
            output: [u32; 2],
            render: [u32; 2],
        ) -> Result<Self, String> {
            let device = context.device().clone();
            let generator = engine_dlss::FrameGenerator::with_context(
                context,
                session,
                engine_dlss::FrameGenerationOptions {
                    output_size: output,
                    render_size: render,
                    color_format: engine_dlss::ColorFormat::Rgba8Unorm,
                    hudless: false,
                },
            )
            .map_err(|e| format!("DLSS Frame Generation initialization failed: {e}"))?;
            let generated = (1..=3)
                .map(|i| ColorTarget::new(&device, &format!("DLSS-G generated {i}"), output))
                .collect();
            Ok(Self {
                output,
                render,
                generator,
                poisoned: false,
                hudless: ColorTarget::new(&device, "DLSS-G HUD-less scene", output),
                backbuffer: ColorTarget::new(&device, "DLSS-G backbuffer", output),
                generated,
                depth: data_texture(
                    &device,
                    "DLSS-G depth",
                    render,
                    wgpu::TextureFormat::R32Float,
                ),
                motion: data_texture(
                    &device,
                    "DLSS-G motion",
                    render,
                    wgpu::TextureFormat::Rg16Float,
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlss::DeviceSupport;
    use engine_project::rendering3d::{Camera3D, FrameGenerationMode};
    use glam::Mat4;

    fn fg(multiplier: u32) -> FrameGenerationSettings {
        FrameGenerationSettings {
            mode: FrameGenerationMode::DlssFg,
            multiplier,
        }
    }

    #[test]
    fn generated_frames_come_first_then_the_real_frame_evenly_spaced() {
        let interval = Duration::from_millis(30);
        assert_eq!(
            presentation_offsets(interval, 1),
            [Duration::ZERO, Duration::from_millis(15)]
        );
        assert_eq!(
            presentation_offsets(interval, 3),
            [0, 7500, 15000, 22500].map(Duration::from_micros)
        );
        assert_eq!(presentation_offsets(interval, 0), [Duration::ZERO]);
        // A long stall is not replayed as long waits.
        let capped = presentation_offsets(Duration::from_secs(2), 1);
        assert_eq!(capped[1], MAX_PRESENT_SPACING);
    }

    #[test]
    fn the_pacer_averages_real_frame_intervals_and_starts_without_delay() {
        let mut pacer = PresentPacer::default();
        let start = Instant::now();
        assert_eq!(pacer.begin(start), Duration::ZERO);
        assert_eq!(
            pacer.begin(start + Duration::from_millis(40)),
            Duration::from_millis(40)
        );
        let next = pacer.begin(start + Duration::from_millis(60));
        assert_eq!(next, Duration::from_millis(35), "(40*3+20)/4");
        pacer.reset();
        assert_eq!(pacer.begin(start), Duration::ZERO);
    }

    #[test]
    fn resets_follow_history_breaks_and_gaps_without_game_frames() {
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let mut state = FrameGeneration::new();
        let now = Instant::now();
        assert!(state.needs_reset(&camera, now), "first frame");
        state.history.commit(&camera);
        state.finish_frame(now);
        assert!(!state.needs_reset(&camera, now + Duration::from_millis(16)));
        assert!(
            state.needs_reset(&camera, now + MAX_GENERATION_GAP + Duration::from_millis(1)),
            "no game frames for longer than the gap"
        );
        state.request_reset();
        assert!(state.needs_reset(&camera, now), "explicit cut");
        state.history.commit(&camera);
        state.off(FrameGenerationSettings::default());
        assert!(state.needs_reset(&camera, now), "frames without generation");
    }

    #[test]
    fn unsupported_requests_report_precise_reasons_and_count_nothing() {
        let mut state = FrameGeneration::new();
        let unavailable = UpscalerDevice(DeviceSupport::Unavailable(
            "DLSS requires the Vulkan backend".into(),
        ));
        let sizes = ([64, 64], [64, 64]);
        let reason = state.precheck(fg(2), &unavailable, None, sizes).unwrap();
        assert_eq!(reason, "DLSS requires the Vulkan backend");
        state.fallback(fg(2), &reason);
        let status = state.status();
        assert_eq!(status.state, FrameGenerationState::Fallback);
        assert_eq!(status.generated_frames, 0);
        assert!(status.multiplier.is_none());
        let unprepared = UpscalerDevice::default();
        let reason = state.precheck(fg(2), &unprepared, None, sizes).unwrap();
        if cfg!(feature = "dlss") {
            assert!(
                reason.contains("NGX") || reason.contains("without DLSS extensions"),
                "{reason}"
            );
        } else {
            assert_eq!(
                reason,
                "engine-desktop was built without the `dlss` feature"
            );
        }
        assert!(
            state
                .precheck(fg(5), &unprepared, None, sizes)
                .unwrap()
                .contains("multiplier must be 2, 3 or 4")
        );
    }

    #[cfg(feature = "dlss")]
    #[test]
    fn overlay_features_and_sticky_failures_block_only_their_request() {
        let mut state = FrameGeneration::new();
        let ready = UpscalerDevice(DeviceSupport::Ready(crate::dlss::DlssPaths {
            project_id: "p".into(),
            runtime_dir: ".".into(),
            log_dir: ".".into(),
            frame_generation: None,
        }));
        let sizes = ([64, 64], [64, 64]);
        assert!(state.precheck(fg(2), &ready, None, sizes).is_none());
        assert!(
            state
                .precheck(
                    fg(2),
                    &ready,
                    Some("overlay render features require replay support"),
                    sizes
                )
                .unwrap()
                .contains("overlay render features")
        );
        state.remember_failure(fg(2), sizes, "runtime missing");
        assert_eq!(
            state.precheck(fg(2), &ready, None, sizes).as_deref(),
            Some("runtime missing")
        );
        assert!(state.precheck(fg(3), &ready, None, sizes).is_none());
        assert!(
            state
                .precheck(fg(2), &ready, None, ([32, 32], [32, 32]))
                .is_none()
        );
    }

    #[test]
    fn interpolation_failure_commits_the_existing_real_source_once() {
        let camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let mut state = FrameGeneration::new();
        let now = Instant::now();
        let sizes = ([64, 64], [64, 64]);
        assert!(!state.complete_evaluation(
            1,
            &camera,
            now,
            fg(2),
            sizes,
            Some("evaluation failed")
        ));
        assert!(!state.complete_evaluation(1, &camera, now, fg(2), sizes, None));
        assert!(!state.complete_evaluation(
            1,
            &camera,
            now,
            fg(2),
            sizes,
            Some("evaluation failed")
        ));
        assert_eq!(
            state.history.frame(),
            1,
            "retain the real source rather than rerendering it"
        );
        assert_eq!(state.status().state, FrameGenerationState::Fallback);
        assert!(state.presentable.is_empty());
        assert!(
            state.needs_reset(&camera, now),
            "a replacement generator resets"
        );
        assert_eq!(state.failure.as_ref().unwrap().1, "evaluation failed");
    }

    #[test]
    fn active_status_reports_the_multiplier_and_counters() {
        let mut state = FrameGeneration::new();
        state.generated = 6;
        state.resets = 1;
        state.active(fg(4), 5);
        state.note_presented(3);
        let status = state.status();
        assert_eq!(status.state, FrameGenerationState::Active);
        assert_eq!(
            (status.multiplier, status.multi_frame_max),
            (Some(4), Some(5))
        );
        assert_eq!(
            (
                status.generated_frames,
                status.presented_frames,
                status.history_resets
            ),
            (6, 3, 1)
        );
    }

    #[test]
    fn row_major_matrices_put_translation_in_the_last_column() {
        let m = Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let rows = row_major(m);
        assert_eq!(rows[0], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(rows[2], [0.0, 0.0, 1.0, 3.0]);
        assert_eq!(rows[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[cfg(feature = "dlss")]
    #[test]
    fn camera_constants_are_unjittered_and_map_current_to_previous_clip() {
        use glam::{Vec3, Vec4};
        let mut camera = CameraInstance3D {
            camera: Camera3D::default(),
            world: Mat4::from_translation(Vec3::new(0.0, 1.0, 5.0)).to_cols_array_2d(),
        };
        let aspect = 16.0 / 9.0;
        let current = crate::camera_matrix(&camera, aspect).unwrap();
        let previous = {
            camera.world = Mat4::from_translation(Vec3::new(-0.5, 1.0, 5.0)).to_cols_array_2d();
            let vp = crate::camera_matrix(&camera, aspect).unwrap();
            camera.world = Mat4::from_translation(Vec3::new(0.0, 1.0, 5.0)).to_cols_array_2d();
            vp
        };
        let projection = super::super::scene3d::camera_projection(&camera, aspect).unwrap();
        let constants = generation_camera(&camera, aspect, projection, current, previous);
        let from_rows = |rows: [[f32; 4]; 4]| Mat4::from_cols_array_2d(&rows).transpose();
        let point = Vec4::new(0.3, 0.2, 0.0, 1.0);
        let expected = previous * point;
        let mapped = from_rows(constants.clip_to_prev_clip) * (current * point);
        assert!(
            (expected - mapped).abs().max_element() < 1e-4,
            "{expected} {mapped}"
        );
        assert!(from_rows(constants.view_to_clip).abs_diff_eq(projection, 1e-6));
        assert_eq!(constants.position, [0.0, 1.0, 5.0]);
        assert_eq!(constants.forward, [0.0, 0.0, -1.0]);
        assert_eq!((constants.near, constants.far), (0.1, 1000.0));
        assert!((constants.fov_y - std::f32::consts::FRAC_PI_3).abs() < 1e-6);
        let frame = engine_dlss::GenerationFrame {
            camera: constants,
            ..Default::default()
        };
        frame.validate().unwrap();
    }
}
