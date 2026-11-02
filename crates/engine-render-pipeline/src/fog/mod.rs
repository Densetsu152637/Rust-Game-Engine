//! Height fog with wind-advected noise.
//!
//! [`FOG_FEATURE`] (`engine.fog`, OpaqueEffects stage) is a participating
//! medium in front of opaque geometry and sky pixels: exponential height
//! fog (analytic optical depth) optionally modulated by 3D value noise that
//! drifts with a wind vector (marched in a bounded number of steps), lit by the
//! view's first directional light (Cornette-Shanks phase, the atmosphere's
//! sun and phase function) plus the view's ambient radiance.
//!
//! The feature is disabled by default and does nothing for a view without
//! [`FogControls`] settings, so absent settings leave output unchanged. It runs
//! after `engine.ssr` and `engine.atmosphere.aerial`. See docs/rendering-fog.md
//! for the model, placement and limits.

mod model;

use crate::context::{FeatureContext, FeatureSetup};
use crate::feature::{FeatureDescriptor, FeatureRegistry, PipelineFeature, Stage};
use crate::shader::SCATTER_WGSL;
use crate::targets::HDR_FORMAT;
use crate::view::ViewId;
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use model::{
    EXPONENT_LIMIT, NOISE_PERIOD, cell_value, fogged, height_optical_depth, height_transmittance,
    modulation, noise_origin, optical_depth, source_radiance, value_noise,
};

/// Fog on opaque geometry and sky pixels (OpaqueEffects stage).
pub const FOG_FEATURE: &str = "engine.fog";

/// Largest density at the base height, in 1/m.
pub const MAX_DENSITY: f32 = 10.0;
/// Largest height falloff, in 1/m.
pub const MAX_HEIGHT_FALLOFF: f32 = 1.0;
/// Largest absolute base height, in metres.
pub const MAX_BASE_HEIGHT: f32 = 1.0e7;
/// Largest sun or ambient intensity multiplier.
pub const MAX_INTENSITY: f32 = 1000.0;
/// Largest distance integrated for sky pixels, in metres.
pub const MAX_SKY_DISTANCE: f32 = 1.0e6;
/// Allowed noise cell sizes, in metres.
pub const NOISE_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=1.0e5;
/// Allowed distances marched with noise, in metres.
pub const NOISE_DISTANCE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=1.0e5;
/// Most noise steps per pixel.
pub const MAX_NOISE_STEPS: u32 = 64;
/// Largest wind speed per axis, in m/s.
pub const MAX_WIND: f32 = 200.0;

/// Physical and noise parameters of one fog. Lengths are metres, rates 1/m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogSettings {
    /// Extinction at the base height, 1/m; zero disables the fog.
    pub density: f32,
    /// Exponential decay of the density with height, 1/m (zero: uniform fog).
    pub height_falloff: f32,
    /// World height at which the density equals `density`, metres.
    pub base_height: f32,
    /// Scattering albedo (fog colour under white light), per channel.
    pub albedo: [f32; 3],
    /// Cornette-Shanks asymmetry in `-0.99..=0.99` (positive: forward).
    pub anisotropy: f32,
    /// Multiplies the sun in-scatter.
    pub sun_intensity: f32,
    /// Multiplies the ambient in-scatter.
    pub ambient_intensity: f32,
    /// Distance integrated for sky pixels (far-plane depth); zero leaves the
    /// sky untouched.
    pub sky_distance: f32,
    /// Strength of the noise modulation `1 +- amount`; zero turns noise off.
    pub noise_amount: f32,
    /// Noise cell size, metres.
    pub noise_scale: f32,
    /// Distance from the camera marched with noise; beyond it the density is
    /// the unmodulated height fog.
    pub noise_distance: f32,
    /// Noise segments per pixel, `1..=64`.
    pub noise_steps: u32,
    /// Selects one of the noise patterns.
    pub noise_seed: u32,
    /// Wind that advects the noise, m/s.
    pub wind: [f32; 3],
}

impl Default for FogSettings {
    fn default() -> Self {
        Self {
            density: 0.01,
            height_falloff: 0.1,
            base_height: 0.0,
            albedo: [0.8, 0.85, 0.9],
            anisotropy: 0.3,
            sun_intensity: 1.0,
            ambient_intensity: 1.0,
            sky_distance: 10_000.0,
            noise_amount: 0.0,
            noise_scale: 20.0,
            noise_distance: 300.0,
            noise_steps: 16,
            noise_seed: 0,
            wind: [0.0; 3],
        }
    }
}

impl FogSettings {
    pub fn validate(&self) -> Result<()> {
        let within = |v: f32, max: f32| v.is_finite() && (0.0..=max).contains(&v);
        ensure!(
            within(self.density, MAX_DENSITY),
            "fog density must be 0..=10 per metre"
        );
        ensure!(
            within(self.height_falloff, MAX_HEIGHT_FALLOFF),
            "fog height falloff must be 0..=1 per metre"
        );
        ensure!(
            self.base_height.is_finite() && self.base_height.abs() <= MAX_BASE_HEIGHT,
            "fog base height must be within +-1e7 metres"
        );
        ensure!(
            self.albedo.into_iter().all(|v| within(v, 1.0)),
            "fog albedo must be 0..=1 per channel"
        );
        ensure!(
            self.anisotropy.is_finite() && self.anisotropy.abs() <= 0.99,
            "fog anisotropy must be -0.99..=0.99"
        );
        ensure!(
            within(self.sun_intensity, MAX_INTENSITY)
                && within(self.ambient_intensity, MAX_INTENSITY),
            "fog sun and ambient intensity must be 0..=1000"
        );
        ensure!(
            within(self.sky_distance, MAX_SKY_DISTANCE),
            "fog sky distance must be 0..=1e6 metres"
        );
        ensure!(
            within(self.noise_amount, 1.0),
            "fog noise amount must be 0..=1"
        );
        ensure!(
            self.noise_scale.is_finite() && NOISE_SCALE_RANGE.contains(&self.noise_scale),
            "fog noise scale must be 0.5..=1e5 metres"
        );
        ensure!(
            self.noise_distance.is_finite() && NOISE_DISTANCE_RANGE.contains(&self.noise_distance),
            "fog noise distance must be 1..=1e5 metres"
        );
        ensure!(
            (1..=MAX_NOISE_STEPS).contains(&self.noise_steps),
            "fog noise steps must be 1..=64"
        );
        ensure!(
            self.wind
                .into_iter()
                .all(|v| v.is_finite() && v.abs() <= MAX_WIND),
            "fog wind must be within +-200 m/s per axis"
        );
        Ok(())
    }

    /// Whether this fog changes any pixel at all.
    pub fn is_visible(&self) -> bool {
        self.density > 0.0
    }
}

/// A fog and where one camera sits in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogFrame {
    pub settings: FogSettings,
    /// Absolute camera position in world coordinates (floating origin
    /// included), metres. Sum the applied origin and the origin-relative
    /// camera position in `f64` before narrowing: see [`absolute_position`].
    pub camera_position: [f64; 3],
    /// Scene clock in seconds. It drives the wind only, so the same time gives
    /// the same fog.
    pub time: f64,
}

impl FogFrame {
    pub fn new(settings: FogSettings, camera_position: [f64; 3], time: f64) -> Result<Self> {
        settings.validate()?;
        ensure!(
            camera_position.into_iter().all(f64::is_finite),
            "camera position must be finite"
        );
        ensure!(time.is_finite(), "fog time must be finite");
        Ok(Self {
            settings,
            camera_position,
            time,
        })
    }

    /// Camera height above the fog's base height, in `f64`.
    pub fn camera_height(&self) -> f64 {
        self.camera_position[1] - f64::from(self.settings.base_height)
    }

    /// The camera in wrapped noise space (see [`noise_origin`]).
    pub fn noise_origin(&self) -> [f64; 3] {
        noise_origin(
            self.camera_position,
            self.settings.wind,
            self.time,
            self.settings.noise_scale,
        )
    }
}

/// Absolute camera position under a floating origin: the applied origin plus
/// the camera's origin-relative position, summed per axis in `f64`.
pub fn absolute_position(applied_origin: [f64; 3], camera_local: [f32; 3]) -> [f64; 3] {
    std::array::from_fn(|k| applied_origin[k] + f64::from(camera_local[k]))
}

/// Cloneable host handle holding per-view fog settings. A view without an
/// entry draws nothing even when the feature is enabled.
#[derive(Clone, Default)]
pub struct FogControls(Arc<RwLock<HashMap<ViewId, FogFrame>>>);

impl FogControls {
    /// Replace (`Some`) or clear (`None`) the fog of `view`. Invalid settings
    /// leave the previous state untouched.
    pub fn set(&self, view: ViewId, frame: Option<FogFrame>) -> Result<()> {
        if let Some(frame) = frame {
            FogFrame::new(frame.settings, frame.camera_position, frame.time)?;
        }
        let mut views = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("fog controls lock poisoned"))?;
        match frame {
            Some(frame) => {
                views.insert(view, frame);
            }
            None => {
                views.remove(&view);
            }
        }
        Ok(())
    }

    pub fn get(&self, view: ViewId) -> Result<Option<FogFrame>> {
        Ok(self
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("fog controls lock poisoned"))?
            .get(&view)
            .copied())
    }

    /// Number of views with settings.
    pub fn view_count(&self) -> usize {
        self.0.read().map_or(0, |views| views.len())
    }
}

/// Register [`FogFeature`] (disabled by default) under its reserved `engine.`
/// name. Register it in the same registry as the atmosphere so the
/// after-aerial-perspective ordering applies.
pub fn register_features(registry: &mut FeatureRegistry, controls: &FogControls) -> Result<()> {
    registry.register_engine(FogFeature::new(controls.clone()))
}

/// `Fog` in `fog.wgsl` (std140: six `vec4`s).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct FogUniform {
    albedo: [f32; 4],
    medium: [f32; 4],
    noise: [f32; 4],
    origin: [f32; 4],
    lighting: [f32; 4],
    seed: [u32; 4],
}

impl FogUniform {
    pub fn new(frame: &FogFrame) -> Self {
        let s = &frame.settings;
        let [r, g, b] = s.albedo;
        let [ox, oy, oz] = frame.noise_origin().map(|v| v as f32);
        Self {
            albedo: [r, g, b, s.anisotropy],
            medium: [
                s.density,
                s.height_falloff,
                frame.camera_height() as f32,
                s.sky_distance,
            ],
            noise: [
                s.noise_amount,
                1.0 / s.noise_scale,
                s.noise_distance,
                s.noise_steps as f32,
            ],
            origin: [ox, oy, oz, 0.0],
            lighting: [s.sun_intensity, s.ambient_intensity, 0.0, 0.0],
            seed: [s.noise_seed, 0, 0, 0],
        }
    }
}

const FOG_WGSL: &str = include_str!("../wgsl/fog.wgsl");

/// The uncomposed WGSL of the pass: view import, noise period, shared
/// scattering helpers, pass.
fn pass_source() -> String {
    let mask = NOISE_PERIOD - 1;
    format!(
        "#import engine::view
const FOG_NOISE_MASK: u32 = {mask}u;
{SCATTER_WGSL}
{FOG_WGSL}"
    )
}

struct Gpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

fn texture_entry(binding: u32, depth: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: if depth {
                wgpu::TextureSampleType::Depth
            } else {
                wgpu::TextureSampleType::Float { filterable: false }
            },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_gpu(setup: &FeatureSetup<'_>) -> Result<Gpu> {
    let device = &setup.gpu.device;
    let label = FOG_FEATURE;
    let source = setup
        .composer
        .compose(&pass_source())
        .context("composing engine.fog")?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<FogUniform>() as u64
                    ),
                },
                count: None,
            },
            texture_entry(1, false),
            texture_entry(2, true),
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&setup.layouts.view), Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
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
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Ok(Gpu { layout, pipeline })
}

/// Height fog with wind-advected noise on opaque geometry and sky pixels.
pub struct FogFeature {
    controls: FogControls,
    gpu: Option<Gpu>,
}

impl FogFeature {
    pub fn new(controls: FogControls) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for FogFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(FOG_FEATURE, Stage::OpaqueEffects)
            // Reflections and decals are already in the scene colour, and the
            // thin atmosphere is applied first: the fog is the medium nearest
            // the camera.
            .after(crate::pipeline::names::SSR)
            .after(crate::atmosphere::ATMOSPHERE_AERIAL_FEATURE)
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.gpu = Some(create_gpu(setup)?);
        Ok(())
    }

    fn forget_view(&mut self, view: ViewId) {
        let _ = self.controls.set(view, None);
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()> {
        let Some(frame) = self.controls.get(ctx.view.id)? else {
            return Ok(());
        };
        frame.settings.validate()?;
        if !frame.settings.is_visible() {
            return Ok(());
        }
        let gpu = self.gpu.as_ref().context("fog not prepared")?;
        let uniform = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(FOG_FEATURE),
            size: std::mem::size_of::<FogUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.gpu
            .queue
            .write_buffer(&uniform, 0, bytemuck::bytes_of(&FogUniform::new(&frame)));
        let depth = ctx.depth()?.view.clone();
        let (source, destination) = ctx.opaque_effect_io()?;
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(FOG_FEATURE),
                layout: &gpu.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&depth),
                    },
                ],
            });
        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(FOG_FEATURE),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, ctx.view_bind_group, &[]);
        pass.set_bind_group(1, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
