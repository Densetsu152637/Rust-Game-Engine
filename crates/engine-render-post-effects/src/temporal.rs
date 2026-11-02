//! Native temporal resolve: anti-aliasing at render scale 1 and temporal
//! upscaling below it, as an `Upscale`-stage feature.
//!
//! The feature consumes the jittered render-size scene colour, depth and
//! motion vectors and writes the output-size HDR image the later stages read,
//! exactly like the built-in bilinear upscale it supersedes
//! ([`engine_render_pipeline::StageRule::LastEnabledWins`]). It is a generic
//! algorithm (reconstruction filter, history reprojection, YCoCg neighbourhood
//! clipping, depth-based disocclusion, 1/n accumulation) and is not FSR, XeSS
//! or DLSS.
//!
//! The host owns the two inputs the pipeline cannot invent: the per-view
//! jitter in [`engine_render_pipeline::ViewData::jitter`] (see
//! [`engine_render_pipeline::jitter`]) and the per-view [`TemporalControls`].
//! History lives in the controls' shared store, so `set(view, None)` (the host
//! switched the view to another mode) releases it immediately, and the
//! pipeline's own `reset_history`, `finish_scene` and `forget_view` callbacks
//! discard, commit and release it.

use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use engine_render_pipeline::{
    FeatureContext, FeatureDescriptor, FeatureNeeds, FeatureSetup, HDR_FORMAT, PipelineFeature,
    Stage, ViewId, names,
};
use glam::Mat4;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Registered feature name. It is driven by the host's upscaler choice, not by
/// authored `render_features`.
pub const TEMPORAL_RESOLVE_FEATURE: &str = "effects.temporal_resolve";
/// Longest accumulation window a view may ask for.
pub const MAX_HISTORY_FRAMES: u32 = 64;
/// Format of the per-pixel history data: view distance, accumulated frames.
pub const HISTORY_AUX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;

const CLIP_GAMMA: f32 = 1.25;
const DEPTH_TOLERANCE: f32 = 0.04;
const HALF_HISTORY_SPEED_PIXELS: f32 = 4.0;

/// Per-view settings of the resolve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemporalResolveSettings {
    /// Most frames accumulated per pixel; the newest frame weighs `1 / n`.
    pub history_frames: u32,
    /// Exponent `k` of the Gaussian reconstruction weight `exp(-k d^2)`, `d`
    /// the distance in render pixels between an output pixel and a jittered
    /// texel. Larger is sharper and noisier; the default 5 is a Gaussian of about 0.32
    /// render pixels.
    pub filter_sharpness: f32,
}

impl Default for TemporalResolveSettings {
    fn default() -> Self {
        Self {
            history_frames: 16,
            filter_sharpness: 5.0,
        }
    }
}

impl TemporalResolveSettings {
    /// The host's settings for a render/output ratio: the accumulation window
    /// covers one jitter period (see [`engine_render_pipeline::jitter`]), at
    /// least 16 and at most [`MAX_HISTORY_FRAMES`] frames.
    pub fn for_sizes(render: [u32; 2], output: [u32; 2]) -> Self {
        Self {
            history_frames: engine_render_pipeline::jitter::jitter_phase_count(render, output)
                .clamp(16, MAX_HISTORY_FRAMES),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_HISTORY_FRAMES).contains(&self.history_frames),
            "temporal history frames must be 1..={MAX_HISTORY_FRAMES}"
        );
        ensure!(
            self.filter_sharpness.is_finite() && (0.5..=12.0).contains(&self.filter_sharpness),
            "temporal filter sharpness must be 0.5..=12"
        );
        Ok(())
    }
}

/// What the resolve did for one view, for status and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TemporalStatus {
    /// A committed history exists for the next frame.
    pub history_valid: bool,
    /// Output size of the live history textures, when allocated.
    pub history_size: Option<[u32; 2]>,
    /// Scene evaluations whose resolve was committed.
    pub resolved_frames: u64,
    /// Times a valid history was discarded (cut, abort, resize, projection).
    pub history_discards: u64,
}

struct Slot {
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    aux_view: wgpu::TextureView,
}

impl Slot {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let make = |label, format| -> (wgpu::Texture, wgpu::TextureView) {
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
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            (texture, view)
        };
        let (color_texture, color_view) = make("temporal history colour", HDR_FORMAT);
        let (_, aux_view) = make("temporal history distance", HISTORY_AUX_FORMAT);
        Self {
            color_texture,
            color_view,
            aux_view,
        }
    }
}

struct History {
    size: [u32; 2],
    slots: [Slot; 2],
    /// Slot holding the committed history; the other receives this frame.
    committed: usize,
    valid: bool,
    /// Camera and projection the committed history was rendered with.
    world_from_view: Mat4,
    clip_from_view: Mat4,
    /// The attempt in flight, committed by `finish_scene`.
    pending: Option<(Mat4, Mat4)>,
}

impl History {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        Self {
            size,
            slots: [Slot::new(device, size), Slot::new(device, size)],
            committed: 0,
            valid: false,
            world_from_view: Mat4::IDENTITY,
            clip_from_view: Mat4::IDENTITY,
            pending: None,
        }
    }
}

#[derive(Default)]
struct ViewEntry {
    settings: Option<TemporalResolveSettings>,
    history: Option<History>,
    resolved_frames: u64,
    discards: u64,
}

impl ViewEntry {
    fn discard(&mut self) {
        if let Some(history) = &mut self.history {
            if history.valid {
                self.discards += 1;
            }
            history.valid = false;
            history.pending = None;
        }
    }
}

#[derive(Default)]
struct Store {
    views: HashMap<ViewId, ViewEntry>,
}

/// Cloneable host handle to the per-view settings and history of the resolve.
#[derive(Clone, Default)]
pub struct TemporalControls(Arc<Mutex<Store>>);

impl TemporalControls {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("temporal controls lock poisoned"))
    }

    /// Declare how the view's Upscale stage is owned this frame: `Some` while
    /// the native temporal resolve runs, `None` for any other owner or mode.
    /// `None` releases the view's history textures at once, so a later
    /// re-enable starts from a fresh resolve. Invalid settings change nothing.
    pub fn set(&self, view: ViewId, settings: Option<TemporalResolveSettings>) -> Result<()> {
        if let Some(settings) = settings {
            settings.validate()?;
        }
        let mut store = self.lock()?;
        match settings {
            Some(settings) => store.views.entry(view).or_default().settings = Some(settings),
            None => {
                if let Some(entry) = store.views.get_mut(&view) {
                    entry.discard();
                    entry.history = None;
                    entry.settings = None;
                }
            }
        }
        Ok(())
    }

    pub fn settings(&self, view: ViewId) -> Result<Option<TemporalResolveSettings>> {
        Ok(self.lock()?.views.get(&view).and_then(|e| e.settings))
    }

    /// Test hook: overwrite one pixel of the committed history colour with raw
    /// `Rgba16Float` bits (for example NaN `0x7e00` or infinity `0x7c00`) to
    /// show that non-finite history never reaches the output. Does nothing when
    /// the view has no committed history; errors when `pixel` is outside it.
    #[doc(hidden)]
    pub fn poison_history_pixel(
        &self,
        queue: &wgpu::Queue,
        view: ViewId,
        pixel: [u32; 2],
        bits: [u16; 4],
    ) -> Result<()> {
        let store = self.lock()?;
        let Some(history) = store
            .views
            .get(&view)
            .and_then(|entry| entry.history.as_ref())
            .filter(|history| history.valid)
        else {
            return Ok(());
        };
        ensure!(
            pixel[0] < history.size[0] && pixel[1] < history.size[1],
            "poisoned pixel {pixel:?} is outside the history"
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &history.slots[history.committed].color_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: pixel[0],
                    y: pixel[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&bits),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn status(&self, view: ViewId) -> Result<TemporalStatus> {
        Ok(self
            .lock()?
            .views
            .get(&view)
            .map(|entry| TemporalStatus {
                history_valid: entry.history.as_ref().is_some_and(|h| h.valid),
                history_size: entry.history.as_ref().map(|h| h.size),
                resolved_frames: entry.resolved_frames,
                history_discards: entry.discards,
            })
            .unwrap_or_default())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    inverse_projection: [[f32; 4]; 4],
    previous_view_from_view: [[f32; 4]; 4],
    sizes: [f32; 4],
    jitter: [f32; 4],
    tuning: [f32; 4],
}

struct Gpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

/// The native temporal resolve as a registered `Upscale` feature.
pub struct TemporalResolveFeature {
    controls: TemporalControls,
    gpu: Option<Gpu>,
}

impl TemporalResolveFeature {
    pub fn new(controls: TemporalControls) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

fn attachment(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            store: wgpu::StoreOp::Store,
        },
    })
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

impl PipelineFeature for TemporalResolveFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(TEMPORAL_RESOLVE_FEATURE, Stage::Upscale)
            .after(names::UPSCALE)
            .needs(FeatureNeeds {
                motion: true,
                ..Default::default()
            })
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        let device = &setup.gpu.device;
        let unfilterable = wgpu::TextureSampleType::Float { filterable: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("temporal resolve"),
            entries: &[
                texture_entry(0, unfilterable),
                texture_entry(1, wgpu::TextureSampleType::Depth),
                texture_entry(2, unfilterable),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: true }),
                texture_entry(4, unfilterable),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<Uniform>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("temporal resolve"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("temporal resolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("temporal.wgsl").into()),
        });
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("temporal resolve"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[
                    target(HDR_FORMAT),
                    target(HDR_FORMAT),
                    target(HISTORY_AUX_FORMAT),
                ],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("temporal history"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        self.gpu = Some(Gpu {
            layout,
            pipeline,
            sampler,
        });
        Ok(())
    }

    fn reset_history(&mut self, view: ViewId) {
        if let Ok(mut store) = self.controls.lock()
            && let Some(entry) = store.views.get_mut(&view)
        {
            entry.discard();
        }
    }

    fn finish_scene(&mut self, view: ViewId, _evaluation_id: u64) {
        if let Ok(mut store) = self.controls.lock()
            && let Some(entry) = store.views.get_mut(&view)
            && let Some(history) = &mut entry.history
            && let Some((world_from_view, clip_from_view)) = history.pending.take()
        {
            history.committed = 1 - history.committed;
            history.valid = true;
            history.world_from_view = world_from_view;
            history.clip_from_view = clip_from_view;
            entry.resolved_frames += 1;
        }
    }

    fn forget_view(&mut self, view: ViewId) {
        if let Ok(mut store) = self.controls.lock() {
            store.views.remove(&view);
        }
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let gpu = self.gpu.as_ref().context("temporal resolve not prepared")?;
        let view = ctx.view;
        let device = ctx.gpu.device.clone();
        let depth = ctx.depth()?.view.clone();
        let motion = ctx
            .motion()
            .context("the temporal resolve requires motion vectors")?
            .view
            .clone();
        let inverse = view.clip_from_view.inverse();
        ensure!(
            inverse.is_finite(),
            "the temporal resolve requires an invertible projection"
        );
        let (source, destination) = ctx.upscale_io()?;

        let mut store = self.controls.lock()?;
        let entry = store.views.entry(view.id).or_default();
        let settings = entry.settings.unwrap_or_default();
        if entry
            .history
            .as_ref()
            .is_some_and(|h| h.size != view.output_size)
        {
            entry.discard();
            entry.history = None;
        }
        if entry
            .history
            .as_ref()
            .is_some_and(|h| h.valid && h.clip_from_view != view.clip_from_view)
        {
            entry.discard();
        }
        let history = entry
            .history
            .get_or_insert_with(|| History::new(&device, view.output_size));
        let valid = history.valid;
        let previous_view_from_view = if valid {
            history.world_from_view.inverse() * view.world_from_view
        } else {
            Mat4::IDENTITY
        };
        let params = Uniform {
            inverse_projection: inverse.to_cols_array_2d(),
            previous_view_from_view: previous_view_from_view.to_cols_array_2d(),
            sizes: [
                view.render_size[0] as f32,
                view.render_size[1] as f32,
                view.output_size[0] as f32,
                view.output_size[1] as f32,
            ],
            jitter: [
                view.jitter[0],
                view.jitter[1],
                if valid { 1.0 } else { 0.0 },
                settings.history_frames as f32,
            ],
            tuning: [
                CLIP_GAMMA,
                DEPTH_TOLERANCE,
                HALF_HISTORY_SPEED_PIXELS,
                settings.filter_sharpness,
            ],
        };
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal resolve params"),
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.gpu
            .queue
            .write_buffer(&uniform, 0, bytemuck::bytes_of(&params));
        let read = &history.slots[history.committed];
        let write = &history.slots[1 - history.committed];
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal resolve"),
            layout: &gpu.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&depth),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&motion),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&read.color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&read.aux_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("temporal resolve"),
                color_attachments: &[
                    attachment(&destination),
                    attachment(&write.color_view),
                    attachment(&write.aux_view),
                ],
                ..Default::default()
            });
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        history.pending = Some((view.world_from_view, view.clip_from_view));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validate_the_history_window() {
        assert!(TemporalResolveSettings::default().validate().is_ok());
        for sharpness in [0.1, 13.0, f32::NAN] {
            assert!(
                TemporalResolveSettings {
                    filter_sharpness: sharpness,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        let low = TemporalResolveSettings::for_sizes([960, 1080], [960, 1080]);
        assert_eq!(low.history_frames, 16);
        let half = TemporalResolveSettings::for_sizes([480, 270], [960, 540]);
        assert_eq!(half.history_frames, 32);
        let third = TemporalResolveSettings::for_sizes([320, 180], [960, 540]);
        assert_eq!(third.history_frames, MAX_HISTORY_FRAMES);
        for frames in [0, MAX_HISTORY_FRAMES + 1] {
            assert!(
                TemporalResolveSettings {
                    history_frames: frames,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            TemporalResolveSettings {
                history_frames: MAX_HISTORY_FRAMES,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn controls_reject_invalid_settings_without_mutation_and_keep_views_apart() {
        let controls = TemporalControls::default();
        let a = ViewId(3);
        let b = ViewId(4);
        controls
            .set(
                a,
                Some(TemporalResolveSettings {
                    history_frames: 8,
                    ..Default::default()
                }),
            )
            .unwrap();
        assert!(
            controls
                .set(
                    a,
                    Some(TemporalResolveSettings {
                        history_frames: 0,
                        ..Default::default()
                    }),
                )
                .is_err()
        );
        assert_eq!(controls.settings(a).unwrap().unwrap().history_frames, 8);
        assert!(controls.settings(b).unwrap().is_none());
        controls.set(a, None).unwrap();
        assert!(controls.settings(a).unwrap().is_none());
        assert_eq!(controls.status(a).unwrap(), TemporalStatus::default());
    }

    #[test]
    fn the_shader_parses_and_validates() {
        engine_render_pipeline::validate_wgsl(include_str!("temporal.wgsl"))
            .expect("temporal.wgsl validates");
    }
}
