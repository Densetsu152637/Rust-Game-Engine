//! Host side of the post stack: evaluates the authored post-processing of
//! a view at its camera position (`engine_project::post::resolve_post`),
//! maps it to the pipeline's [`PostSettings`], caches parsed grading LUTs
//! and measures the frame delta for auto-exposure.

use anyhow::{Context, Result};
use engine_project::post::{self as authored, PostStatus, ResolvedPost};
use engine_project::rendering3d::{RenderPipelineState, SceneRenderFrame};
use engine_render_pipeline::{
    BloomSettings, ExposureMode, ExposureSettings, GradingSettings, GrainSettings, Lut3d,
    PostSettings, TonemapOperator, ViewData, ViewId, VignetteSettings,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

/// Longest frame delta auto-exposure integrates (a stall must not snap).
const MAX_DELTA_SECONDS: f32 = 0.25;

struct CachedLut {
    fingerprint: u64,
    lut: Result<Arc<Lut3d>, String>,
}

#[derive(Default)]
pub(crate) struct PostHost {
    luts: HashMap<String, CachedLut>,
    last_frame: HashMap<ViewId, Instant>,
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The pipeline settings of the authored `post`.
pub(crate) fn pipeline_settings(
    post: &authored::PostProcess,
    lut: Option<Arc<Lut3d>>,
) -> PostSettings {
    let e = &post.exposure;
    let b = &post.bloom;
    let g = &post.grading;
    let v = &post.vignette;
    let n = &post.grain;
    PostSettings {
        tonemap: match post.tonemap {
            authored::TonemapMode::None => TonemapOperator::None,
            authored::TonemapMode::Aces => TonemapOperator::Aces,
            authored::TonemapMode::Agx => TonemapOperator::Agx,
            authored::TonemapMode::AgxPunchy => TonemapOperator::AgxPunchy,
            authored::TonemapMode::Neutral => TonemapOperator::Neutral,
        },
        exposure: ExposureSettings {
            mode: match e.mode {
                authored::ExposureMode::Off => ExposureMode::Off,
                authored::ExposureMode::Manual => ExposureMode::Manual,
                authored::ExposureMode::Auto => ExposureMode::Auto,
            },
            ev100: e.ev100,
            compensation: e.compensation,
            min_ev: e.min_ev,
            max_ev: e.max_ev,
            low_percentile: e.low_percentile,
            high_percentile: e.high_percentile,
            speed_up: e.speed_up,
            speed_down: e.speed_down,
        },
        bloom: BloomSettings {
            enabled: b.enabled,
            threshold: b.threshold,
            knee: b.knee,
            intensity: b.intensity,
            scatter: b.scatter,
            tint: b.tint,
            ..Default::default()
        },
        grading: GradingSettings {
            enabled: g.enabled,
            temperature: g.temperature,
            tint: g.tint,
            exposure_offset: g.exposure_offset,
            contrast: g.contrast,
            saturation: g.saturation,
            lift: g.lift,
            gamma: g.gamma,
            gain: g.gain,
            hue_shift_degrees: g.hue_shift_degrees,
            lut,
            lut_intensity: g.lut_intensity,
        },
        vignette: VignetteSettings {
            enabled: v.enabled,
            intensity: v.intensity,
            smoothness: v.smoothness,
            center: v.center,
            color: v.color,
        },
        grain: GrainSettings {
            enabled: n.enabled,
            intensity: n.intensity,
            response: n.response,
        },
    }
}

impl PostHost {
    /// The parsed LUT `id` of `frame`, from the cache while its text is
    /// unchanged.
    fn lut(&mut self, frame: &SceneRenderFrame, id: &str) -> Result<Arc<Lut3d>, String> {
        let text = frame
            .post
            .luts
            .get(id)
            .ok_or_else(|| format!("the frame carries no file for LUT asset {id}"))?;
        let print = fingerprint(text);
        match self.luts.get(id) {
            Some(cached) if cached.fingerprint == print => cached.lut.clone(),
            _ => {
                let lut = Lut3d::parse_cube(text)
                    .map(Arc::new)
                    .map_err(|e| format!("{id}: {e:#}"));
                self.luts.insert(
                    id.to_owned(),
                    CachedLut {
                        fingerprint: print,
                        lut: lut.clone(),
                    },
                );
                lut
            }
        }
    }

    /// Resolve the view's post-processing, set it on `view` (and the frame
    /// delta for auto-exposure) and return the status to report.
    pub fn prepare(&mut self, frame: &SceneRenderFrame, view: &mut ViewData) -> Result<PostStatus> {
        let position = view.camera_position().to_array();
        let resolved: Option<ResolvedPost> =
            authored::resolve_post(frame.post.camera.as_ref(), &frame.post.volumes, position);
        let mut status = PostStatus::of(resolved.as_ref(), frame.post.volumes.len());
        status.state = RenderPipelineState::Rendered;
        let now = Instant::now();
        let delta = self
            .last_frame
            .insert(view.id, now)
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32())
            .min(MAX_DELTA_SECONDS);
        if view.delta_time == 0.0 {
            view.delta_time = delta;
        }
        if view.post.is_some() {
            return Ok(status);
        }
        let Some(resolved) = resolved else {
            return Ok(status);
        };
        let grading = &resolved.settings.grading;
        let lut = match grading.lut.as_deref().filter(|_| grading.enabled) {
            Some(id) => match self.lut(frame, id) {
                Ok(lut) => Some(lut),
                Err(error) => {
                    status.lut_error = Some(error);
                    None
                }
            },
            None => None,
        };
        let settings = pipeline_settings(&resolved.settings, lut);
        settings
            .validate()
            .context("the resolved post-processing settings are invalid")?;
        view.post = Some(settings);
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_values_map_one_to_one() {
        let mut post = authored::PostProcess::default();
        post.tonemap = authored::TonemapMode::AgxPunchy;
        post.exposure.mode = authored::ExposureMode::Auto;
        post.exposure.compensation = 0.5;
        post.bloom.enabled = true;
        post.bloom.tint = [1.0, 0.5, 0.25];
        post.grading.contrast = 1.4;
        post.vignette.center = [0.4, 0.6];
        post.grain.intensity = 0.3;
        let s = pipeline_settings(&post, None);
        assert_eq!(s.tonemap, TonemapOperator::AgxPunchy);
        assert_eq!(s.exposure.mode, ExposureMode::Auto);
        assert_eq!(s.exposure.compensation, 0.5);
        assert!(s.bloom.enabled && s.bloom.tint == [1.0, 0.5, 0.25]);
        assert_eq!(s.grading.contrast, 1.4);
        assert_eq!(s.vignette.center, [0.4, 0.6]);
        assert_eq!(s.grain.intensity, 0.3);
        s.validate().unwrap();
    }

    #[test]
    fn an_invalid_lut_is_reported_and_not_applied() {
        let mut frame = SceneRenderFrame::from(engine_project::runtime::RenderFrame {
            camera: Default::default(),
            draws: vec![],
        });
        frame
            .post
            .luts
            .insert("bad".into(), Arc::from("LUT_3D_SIZE 2\n0 0 0\n"));
        let mut host = PostHost::default();
        let error = host.lut(&frame, "bad").unwrap_err();
        assert!(error.contains("expected 8 entries"), "{error}");
        assert!(host.lut(&frame, "missing").is_err());
    }
}
