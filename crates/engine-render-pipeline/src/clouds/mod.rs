//! Volumetric clouds.
//!
//! [`CLOUDS_FEATURE`] (`engine.clouds`, OpaqueEffects stage) ray-marches a
//! flat cloud layer between two world altitudes in a bounded number of steps:
//! Beer-Lambert extinction from coverage-remapped fractal noise times a height
//! profile, a bounded light march toward the view's first directional light
//! for self-shadowing, a dual-lobe Cornette-Shanks phase with a documented
//! multiple-scattering approximation, and the view's ambient radiance. The
//! noise drifts with a wind vector and the scene clock.
//!
//! The pass reads the scene colour (sky and aerial perspective already in it)
//! and the scene depth: geometry nearer than the layer hides it, geometry
//! inside it limits the march, and clouds in front of farther geometry cover
//! it. It runs after `engine.ssr` and `engine.atmosphere.aerial` and before
//! `engine.fog`. The feature is disabled by default and does nothing for a
//! view without [`CloudControls`] settings. See docs/rendering-clouds.md for
//! the model, placement and limits.

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
    AMBIENT_BASE, BACK_ANISOTROPY, BACK_WEIGHT, CUTOFF_TRANSMITTANCE, CloudMarch, FLAT_DIRECTION,
    MAX_OCTAVES, MS_FACTOR, MS_OCTAVES, PROFILE_FALL, PROFILE_INTEGRAL, PROFILE_RISE, SUN_HORIZON,
    composite, extinction, fade, fbm, layer_interval, light_depth, march, phase, profile, remap,
    slab_transmittance, source,
};

/// Volumetric clouds over sky and geometry (OpaqueEffects stage).
pub const CLOUDS_FEATURE: &str = "engine.clouds";

/// Largest absolute altitude of the layer, in metres.
pub const MAX_ALTITUDE: f32 = 1.0e7;
/// Thickest layer, in metres.
pub const MAX_THICKNESS: f32 = 1.0e5;
/// Largest density at full coverage, in 1/m.
pub const MAX_DENSITY: f32 = 1.0;
/// Largest sun or ambient intensity multiplier.
pub const MAX_INTENSITY: f32 = 1000.0;
/// Allowed noise cell sizes, in metres.
pub const NOISE_SCALE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=1.0e6;
/// Allowed march distances, in metres.
pub const MAX_DISTANCE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=1.0e6;
/// Largest horizon fade distance, in metres.
pub const MAX_FADE_DISTANCE: f32 = 1.0e7;
/// Most primary march steps per pixel.
pub const MAX_PRIMARY_STEPS: u32 = 96;
/// Most light-march steps per primary sample.
pub const MAX_LIGHT_STEPS: u32 = 12;
/// Allowed light-march lengths, in metres.
pub const LIGHT_DISTANCE_RANGE: std::ops::RangeInclusive<f32> = 1.0..=1.0e5;
/// Largest wind speed per axis, in m/s.
pub const MAX_WIND: f32 = 200.0;

/// Physical and noise parameters of one cloud layer. Lengths are metres,
/// rates 1/m, altitudes are world heights (floating origin included).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSettings {
    /// Altitude of the bottom of the layer.
    pub base_altitude: f32,
    /// Altitude of the top of the layer; must exceed `base_altitude`.
    pub top_altitude: f32,
    /// Extinction at full density, 1/m; zero disables the clouds.
    pub density: f32,
    /// Fraction of the layer that holds cloud, `0..=1`; zero disables them.
    pub coverage: f32,
    /// Scattering albedo per channel (clouds are close to one).
    pub albedo: [f32; 3],
    /// Forward Cornette-Shanks asymmetry in `-0.99..=0.99`.
    pub anisotropy: f32,
    /// Multiplies the sun in-scatter.
    pub sun_intensity: f32,
    /// Multiplies the ambient in-scatter.
    pub ambient_intensity: f32,
    /// Whether the cheap multiple-scattering approximation brightens the
    /// cloud interior.
    pub multiple_scattering: bool,
    /// Strength of the noise `mix(1, fbm, amount)`; zero is a uniform layer.
    pub noise_amount: f32,
    /// Size of the base noise cell, metres.
    pub noise_scale: f32,
    /// Noise octaves, `1..=4`.
    pub noise_octaves: u32,
    /// Selects one of the noise patterns.
    pub noise_seed: u32,
    /// Wind that advects the noise, m/s.
    pub wind: [f32; 3],
    /// Distance the primary march covers at most, metres.
    pub max_distance: f32,
    /// Horizon fade: cloud opacity is scaled by `exp(-entry / fade_distance)`;
    /// zero turns it off.
    pub fade_distance: f32,
    /// Midpoint samples per pixel, `1..=96`.
    pub primary_steps: u32,
    /// Midpoint samples toward the sun per primary sample, `1..=12`.
    pub light_steps: u32,
    /// Length of the light march, metres.
    pub light_distance: f32,
}

impl Default for CloudSettings {
    fn default() -> Self {
        Self {
            base_altitude: 1500.0,
            top_altitude: 3500.0,
            density: 0.004,
            coverage: 0.5,
            albedo: [1.0; 3],
            anisotropy: 0.6,
            sun_intensity: 1.0,
            ambient_intensity: 1.0,
            multiple_scattering: true,
            noise_amount: 1.0,
            noise_scale: 1500.0,
            noise_octaves: 3,
            noise_seed: 0,
            wind: [0.0; 3],
            max_distance: 40_000.0,
            fade_distance: 0.0,
            primary_steps: 48,
            light_steps: 6,
            light_distance: 800.0,
        }
    }
}

impl CloudSettings {
    /// Layer thickness in metres, computed in `f64`.
    pub fn thickness(&self) -> f64 {
        f64::from(self.top_altitude) - f64::from(self.base_altitude)
    }

    pub fn validate(&self) -> Result<()> {
        let within = |v: f32, max: f32| v.is_finite() && (0.0..=max).contains(&v);
        ensure!(
            self.base_altitude.is_finite()
                && self.top_altitude.is_finite()
                && self.base_altitude.abs() <= MAX_ALTITUDE
                && self.top_altitude.abs() <= MAX_ALTITUDE,
            "cloud altitudes must be within +-1e7 metres"
        );
        ensure!(
            self.thickness() >= 1.0 && self.thickness() <= f64::from(MAX_THICKNESS),
            "cloud top must exceed the base by 1 to 1e5 metres"
        );
        ensure!(
            within(self.density, MAX_DENSITY),
            "cloud density must be 0..=1 per metre"
        );
        ensure!(within(self.coverage, 1.0), "cloud coverage must be 0..=1");
        ensure!(
            self.albedo.into_iter().all(|v| within(v, 1.0)),
            "cloud albedo must be 0..=1 per channel"
        );
        ensure!(
            self.anisotropy.is_finite() && self.anisotropy.abs() <= 0.99,
            "cloud anisotropy must be -0.99..=0.99"
        );
        ensure!(
            within(self.sun_intensity, MAX_INTENSITY)
                && within(self.ambient_intensity, MAX_INTENSITY),
            "cloud sun and ambient intensity must be 0..=1000"
        );
        ensure!(
            within(self.noise_amount, 1.0),
            "cloud noise amount must be 0..=1"
        );
        ensure!(
            self.noise_scale.is_finite() && NOISE_SCALE_RANGE.contains(&self.noise_scale),
            "cloud noise scale must be 1..=1e6 metres"
        );
        ensure!(
            (1..=MAX_OCTAVES).contains(&self.noise_octaves),
            "cloud noise octaves must be 1..=4"
        );
        ensure!(
            self.wind
                .into_iter()
                .all(|v| v.is_finite() && v.abs() <= MAX_WIND),
            "cloud wind must be within +-200 m/s per axis"
        );
        ensure!(
            self.max_distance.is_finite() && MAX_DISTANCE_RANGE.contains(&self.max_distance),
            "cloud max distance must be 1..=1e6 metres"
        );
        ensure!(
            within(self.fade_distance, MAX_FADE_DISTANCE),
            "cloud fade distance must be 0..=1e7 metres"
        );
        ensure!(
            (1..=MAX_PRIMARY_STEPS).contains(&self.primary_steps),
            "cloud primary steps must be 1..=96"
        );
        ensure!(
            (1..=MAX_LIGHT_STEPS).contains(&self.light_steps),
            "cloud light steps must be 1..=12"
        );
        ensure!(
            self.light_distance.is_finite() && LIGHT_DISTANCE_RANGE.contains(&self.light_distance),
            "cloud light distance must be 1..=1e5 metres"
        );
        Ok(())
    }

    /// Whether these clouds change any pixel at all.
    pub fn is_visible(&self) -> bool {
        self.density > 0.0 && self.coverage > 0.0
    }
}

/// A cloud layer and where one camera sits relative to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudFrame {
    pub settings: CloudSettings,
    /// Absolute camera position in world coordinates (floating origin
    /// included), metres. See [`absolute_position`].
    pub camera_position: [f64; 3],
    /// Scene clock in seconds. It drives the wind only.
    pub time: f64,
}

impl CloudFrame {
    pub fn new(settings: CloudSettings, camera_position: [f64; 3], time: f64) -> Result<Self> {
        settings.validate()?;
        ensure!(
            camera_position.into_iter().all(f64::is_finite),
            "camera position must be finite"
        );
        ensure!(time.is_finite(), "cloud time must be finite");
        Ok(Self {
            settings,
            camera_position,
            time,
        })
    }

    /// Camera height above the base of the layer, in `f64`.
    pub fn camera_height(&self) -> f64 {
        self.camera_position[1] - f64::from(self.settings.base_altitude)
    }

    /// The camera in wrapped noise space (shared with the fog).
    pub fn noise_origin(&self) -> [f64; 3] {
        model::noise_origin(
            self.camera_position,
            self.settings.wind,
            self.time,
            self.settings.noise_scale,
        )
    }
}

pub use crate::fog::absolute_position;

/// Cloneable host handle holding per-view cloud settings. A view without an
/// entry draws nothing even when the feature is enabled.
#[derive(Clone, Default)]
pub struct CloudControls(Arc<RwLock<HashMap<ViewId, CloudFrame>>>);

impl CloudControls {
    /// Replace (`Some`) or clear (`None`) the clouds of `view`. Invalid
    /// settings leave the previous state untouched.
    pub fn set(&self, view: ViewId, frame: Option<CloudFrame>) -> Result<()> {
        if let Some(frame) = frame {
            CloudFrame::new(frame.settings, frame.camera_position, frame.time)?;
        }
        let mut views = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("cloud controls lock poisoned"))?;
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

    pub fn get(&self, view: ViewId) -> Result<Option<CloudFrame>> {
        Ok(self
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("cloud controls lock poisoned"))?
            .get(&view)
            .copied())
    }

    /// Number of views with settings.
    pub fn view_count(&self) -> usize {
        self.0.read().map_or(0, |views| views.len())
    }
}

/// Register [`CloudFeature`] (disabled by default) under its reserved
/// `engine.` name. Register it in the same registry as the atmosphere and the
/// fog so the ordering constraints apply.
pub fn register_features(registry: &mut FeatureRegistry, controls: &CloudControls) -> Result<()> {
    registry.register_engine(CloudFeature::new(controls.clone()))
}

/// `Clouds` in `clouds.wgsl` (std140: seven `vec4`s).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CloudUniform {
    albedo: [f32; 4],
    layer: [f32; 4],
    density: [f32; 4],
    steps: [f32; 4],
    origin: [f32; 4],
    lighting: [f32; 4],
    seed: [u32; 4],
}

impl CloudUniform {
    pub fn new(frame: &CloudFrame) -> Self {
        let s = &frame.settings;
        let [r, g, b] = s.albedo;
        let [ox, oy, oz] = frame.noise_origin().map(|v| v as f32);
        Self {
            albedo: [r, g, b, s.anisotropy],
            layer: [
                s.thickness() as f32,
                frame.camera_height() as f32,
                s.max_distance,
                s.fade_distance,
            ],
            density: [s.density, s.coverage, s.noise_amount, 1.0 / s.noise_scale],
            steps: [
                s.noise_octaves as f32,
                s.primary_steps as f32,
                s.light_steps as f32,
                s.light_distance,
            ],
            origin: [ox, oy, oz, if s.multiple_scattering { 1.0 } else { 0.0 }],
            lighting: [s.sun_intensity, s.ambient_intensity, 0.0, 0.0],
            seed: [s.noise_seed, 0, 0, 0],
        }
    }
}

const CLOUDS_WGSL: &str = include_str!("../wgsl/clouds.wgsl");

/// The uncomposed WGSL of the pass: view import, constants shared with the CPU
/// reference, scattering helpers, pass.
fn pass_source() -> String {
    let mask = model::NOISE_PERIOD - 1;
    format!(
        "#import engine::view
const CLOUD_NOISE_MASK: u32 = {mask}u;
const CLOUD_PROFILE_RISE: f32 = {:?};
const CLOUD_PROFILE_FALL: f32 = {:?};
const CLOUD_BACK_ANISOTROPY: f32 = {:?};
const CLOUD_BACK_WEIGHT: f32 = {:?};
const CLOUD_MS_OCTAVES: u32 = {}u;
const CLOUD_MS_FACTOR: f32 = {:?};
const CLOUD_AMBIENT_BASE: f32 = {:?};
const CLOUD_CUTOFF: f32 = {:?};
const CLOUD_FLAT_DIRECTION: f32 = {:?};
const CLOUD_SUN_HORIZON: f32 = {:?};
{SCATTER_WGSL}
{CLOUDS_WGSL}",
        PROFILE_RISE,
        PROFILE_FALL,
        BACK_ANISOTROPY,
        BACK_WEIGHT,
        MS_OCTAVES,
        MS_FACTOR,
        AMBIENT_BASE,
        CUTOFF_TRANSMITTANCE,
        FLAT_DIRECTION,
        SUN_HORIZON,
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
    let label = CLOUDS_FEATURE;
    let source = setup
        .composer
        .compose(&pass_source())
        .context("composing engine.clouds")?;
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
                        std::mem::size_of::<CloudUniform>() as u64
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

/// Volumetric clouds over opaque geometry and sky pixels.
pub struct CloudFeature {
    controls: CloudControls,
    gpu: Option<Gpu>,
}

impl CloudFeature {
    pub fn new(controls: CloudControls) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for CloudFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(CLOUDS_FEATURE, Stage::OpaqueEffects)
            // The sky and the aerial perspective of geometry are already in
            // the scene colour; the fog is applied over the clouds.
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
        let gpu = self.gpu.as_ref().context("clouds not prepared")?;
        let uniform = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(CLOUDS_FEATURE),
            size: std::mem::size_of::<CloudUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.gpu
            .queue
            .write_buffer(&uniform, 0, bytemuck::bytes_of(&CloudUniform::new(&frame)));
        let depth = ctx.depth()?.view.clone();
        let (source, destination) = ctx.opaque_effect_io()?;
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(CLOUDS_FEATURE),
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
            label: Some(CLOUDS_FEATURE),
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
