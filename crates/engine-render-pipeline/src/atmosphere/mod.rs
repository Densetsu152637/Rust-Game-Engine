//! Physically based atmospheric sky and aerial perspective.
//!
//! Two built-in features share one single-scattering model (Rayleigh + Mie +
//! ozone absorption around a sphere of configurable radius, driven by the
//! view's first directional light):
//!
//! * [`ATMOSPHERE_FEATURE`] (`engine.atmosphere`, Sky stage): sky radiance, the
//!   planet below the horizon and a sun disk, added where scene depth is still
//!   at the far plane.
//! * [`ATMOSPHERE_AERIAL_FEATURE`] (`engine.atmosphere.aerial`, OpaqueEffects
//!   stage): opaque geometry becomes `scene * transmittance + in-scatter`
//!   along its view distance.
//!
//! Both are disabled by default and do nothing for a view without
//! [`AtmosphereControls`] settings, so absent settings leave output unchanged.
//! See docs/rendering-atmosphere.md for the model, placement and limits.

mod model;

use crate::context::{FeatureContext, FeatureSetup};
use crate::feature::{FeatureDescriptor, FeatureRegistry, PipelineFeature, Stage};
use crate::targets::{DEPTH_FORMAT, HDR_FORMAT};
use crate::view::ViewId;
use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use model::{
    AERIAL_STEPS, BLOCKED_DEPTH, MAX_RADIANCE, Medium, SKY_STEPS, SUN_STEPS, aerial, dot, interval,
    mie_phase, normalize, path_transmittance, rayleigh_phase, sky_radiance, sun_depth,
};

/// Sky, planet and sun disk (Sky stage).
pub const ATMOSPHERE_FEATURE: &str = "engine.atmosphere";
/// Aerial perspective on opaque geometry (OpaqueEffects stage).
pub const ATMOSPHERE_AERIAL_FEATURE: &str = "engine.atmosphere.aerial";

/// Smallest and largest planet radius, in metres.
pub const PLANET_RADIUS_RANGE: std::ops::RangeInclusive<f32> = 1.0e3..=1.0e8;
/// Smallest and largest atmosphere thickness, in metres.
pub const THICKNESS_RANGE: std::ops::RangeInclusive<f64> = 100.0..=1.0e7;
/// Largest scattering or absorption coefficient, in 1/m.
pub const MAX_COEFFICIENT: f32 = 1.0e-2;
/// Allowed scale heights, in metres.
pub const SCALE_HEIGHT_RANGE: std::ops::RangeInclusive<f32> = 1.0..=1.0e6;
/// Largest sun angular diameter, in radians (about 28.6 degrees).
pub const MAX_SUN_DIAMETER: f32 = 0.5;
/// Largest intensity multiplier.
pub const MAX_INTENSITY: f32 = 1000.0;
/// Largest aerial-perspective distance scale.
pub const MAX_AERIAL_SCALE: f32 = 1000.0;

/// Physical parameters of one atmosphere. Lengths are metres, coefficients
/// 1/m. Defaults are an Earth-like atmosphere (Hillaire 2020 coefficients).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereSettings {
    pub planet_radius: f32,
    /// Radius of the top of the atmosphere; must exceed `planet_radius`.
    pub atmosphere_radius: f32,
    /// Rayleigh scattering at the ground, per colour channel.
    pub rayleigh_scattering: [f32; 3],
    pub rayleigh_scale_height: f32,
    pub mie_scattering: f32,
    pub mie_absorption: f32,
    pub mie_scale_height: f32,
    /// Cornette-Shanks asymmetry in `-0.99..=0.99` (positive: forward).
    pub mie_anisotropy: f32,
    /// Ozone absorption at the centre of its tent-shaped layer.
    pub ozone_absorption: [f32; 3],
    pub ozone_center_height: f32,
    /// Distance from the layer centre to where ozone density reaches zero.
    pub ozone_width: f32,
    pub ground_albedo: [f32; 3],
    /// Angular diameter of the sun disk in radians; zero hides the disk.
    pub sun_angular_diameter: f32,
    /// Multiplies the sun irradiance seen by sky, ground and aerial scattering.
    pub intensity: f32,
    /// Multiplies geometry distance for aerial perspective; zero disables it,
    /// one is physical.
    pub aerial_perspective_scale: f32,
}

impl Default for AtmosphereSettings {
    fn default() -> Self {
        Self {
            planet_radius: 6_360_000.0,
            atmosphere_radius: 6_460_000.0,
            rayleigh_scattering: [5.802e-6, 13.558e-6, 33.1e-6],
            rayleigh_scale_height: 8000.0,
            mie_scattering: 3.996e-6,
            mie_absorption: 0.444e-6,
            mie_scale_height: 1200.0,
            mie_anisotropy: 0.8,
            ozone_absorption: [0.650e-6, 1.881e-6, 0.085e-6],
            ozone_center_height: 25_000.0,
            ozone_width: 15_000.0,
            ground_albedo: [0.3; 3],
            sun_angular_diameter: 0.0093,
            intensity: 1.0,
            aerial_perspective_scale: 1.0,
        }
    }
}

impl AtmosphereSettings {
    /// Atmosphere thickness in metres, computed in `f64` so a thin shell on a
    /// large planet keeps its precision.
    pub fn thickness(&self) -> f64 {
        f64::from(self.atmosphere_radius) - f64::from(self.planet_radius)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.planet_radius.is_finite()
                && self.atmosphere_radius.is_finite()
                && PLANET_RADIUS_RANGE.contains(&self.planet_radius),
            "planet radius must be 1e3..=1e8 metres"
        );
        ensure!(
            THICKNESS_RANGE.contains(&self.thickness()),
            "atmosphere radius must exceed the planet radius by 100 m to 1e7 m"
        );
        let coefficient = |v: f32| v.is_finite() && (0.0..=MAX_COEFFICIENT).contains(&v);
        ensure!(
            self.rayleigh_scattering.into_iter().all(coefficient)
                && coefficient(self.mie_scattering)
                && coefficient(self.mie_absorption)
                && self.ozone_absorption.into_iter().all(coefficient),
            "scattering and absorption coefficients must be 0..=0.01 per metre"
        );
        ensure!(
            SCALE_HEIGHT_RANGE.contains(&self.rayleigh_scale_height)
                && SCALE_HEIGHT_RANGE.contains(&self.mie_scale_height),
            "scale heights must be 1..=1e6 metres"
        );
        ensure!(
            self.mie_anisotropy.is_finite() && self.mie_anisotropy.abs() <= 0.99,
            "Mie anisotropy must be -0.99..=0.99"
        );
        ensure!(
            self.ozone_center_height.is_finite()
                && (0.0..=1.0e7).contains(&self.ozone_center_height)
                && self.ozone_width.is_finite()
                && (1.0..=1.0e7).contains(&self.ozone_width),
            "ozone layer centre must be 0..=1e7 metres and its width 1..=1e7 metres"
        );
        ensure!(
            self.ground_albedo
                .into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
            "ground albedo must be 0..=1 per channel"
        );
        ensure!(
            self.sun_angular_diameter.is_finite()
                && (0.0..=MAX_SUN_DIAMETER).contains(&self.sun_angular_diameter),
            "sun angular diameter must be 0..=0.5 radians"
        );
        ensure!(
            self.intensity.is_finite() && (0.0..=MAX_INTENSITY).contains(&self.intensity),
            "atmosphere intensity must be 0..=1000"
        );
        ensure!(
            self.aerial_perspective_scale.is_finite()
                && (0.0..=MAX_AERIAL_SCALE).contains(&self.aerial_perspective_scale),
            "aerial perspective scale must be 0..=1000"
        );
        Ok(())
    }
}

/// An atmosphere and where one camera sits in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereFrame {
    pub settings: AtmosphereSettings,
    /// Camera altitude above the planet surface (world `y = 0`), in metres.
    /// Computed from absolute coordinates: see [`absolute_altitude`].
    pub camera_altitude: f64,
}

impl AtmosphereFrame {
    pub fn new(settings: AtmosphereSettings, camera_altitude: f64) -> Result<Self> {
        settings.validate()?;
        ensure!(
            camera_altitude.is_finite(),
            "camera altitude must be finite"
        );
        Ok(Self {
            settings,
            camera_altitude,
        })
    }

    /// The `f64` medium the CPU reference integrates (see [`sky_radiance`]).
    pub fn medium(&self) -> Medium {
        Medium::new(&self.settings)
    }

    /// Camera altitude as the shader sees it: never below the surface.
    pub fn shader_altitude(&self) -> f32 {
        self.camera_altitude.max(0.0) as f32
    }
}

/// Absolute camera altitude under a floating origin: the applied origin's `y`
/// plus the camera's origin-relative `y`, summed in `f64` before narrowing, so
/// rebasing never changes the sky.
pub fn absolute_altitude(applied_origin_y: f64, camera_local_y: f32) -> f64 {
    applied_origin_y + f64::from(camera_local_y)
}

/// Cloneable host handle holding per-view atmosphere settings. A view without
/// an entry draws nothing even when its features are enabled.
#[derive(Clone, Default)]
pub struct AtmosphereControls(Arc<RwLock<HashMap<ViewId, AtmosphereFrame>>>);

impl AtmosphereControls {
    /// Replace (`Some`) or clear (`None`) the settings of `view`. Invalid
    /// settings leave the previous state untouched.
    pub fn set(&self, view: ViewId, frame: Option<AtmosphereFrame>) -> Result<()> {
        if let Some(frame) = frame {
            AtmosphereFrame::new(frame.settings, frame.camera_altitude)?;
        }
        let mut views = self
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("atmosphere controls lock poisoned"))?;
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

    pub fn get(&self, view: ViewId) -> Result<Option<AtmosphereFrame>> {
        Ok(self
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("atmosphere controls lock poisoned"))?
            .get(&view)
            .copied())
    }

    /// Number of views with settings.
    pub fn view_count(&self) -> usize {
        self.0.read().map_or(0, |views| views.len())
    }
}

/// Register [`AtmosphereFeature`] and [`AerialPerspectiveFeature`] (both
/// disabled by default) under their reserved `engine.` names.
pub fn register_features(
    registry: &mut FeatureRegistry,
    controls: &AtmosphereControls,
) -> Result<()> {
    registry.register_engine(AtmosphereFeature::new(controls.clone()))?;
    registry.register_engine(AerialPerspectiveFeature::new(controls.clone()))
}

/// `Atmosphere` in `atmosphere.wgsl` (std140: six `vec4`s).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct AtmosphereUniform {
    rayleigh: [f32; 4],
    mie: [f32; 4],
    ozone: [f32; 4],
    layer: [f32; 4],
    ground: [f32; 4],
    geometry: [f32; 4],
}

impl AtmosphereUniform {
    pub fn new(frame: &AtmosphereFrame) -> Self {
        let s = &frame.settings;
        let [or, og, ob] = s.ozone_absorption;
        let [ar, ag, ab] = s.ground_albedo;
        let [rr, rg, rb] = s.rayleigh_scattering;
        Self {
            rayleigh: [rr, rg, rb, s.rayleigh_scale_height],
            mie: [
                s.mie_scattering,
                s.mie_absorption,
                s.mie_scale_height,
                s.mie_anisotropy,
            ],
            ozone: [or, og, ob, 0.0],
            layer: [
                s.ozone_center_height,
                s.ozone_width,
                s.aerial_perspective_scale,
                0.5 * s.sun_angular_diameter,
            ],
            ground: [ar, ag, ab, s.intensity],
            geometry: [
                s.planet_radius,
                s.thickness() as f32,
                frame.shader_altitude(),
                0.0,
            ],
        }
    }
}

const COMMON_WGSL: &str = include_str!("../wgsl/atmosphere.wgsl");
const SKY_WGSL: &str = include_str!("../wgsl/atmosphere_sky.wgsl");
const AERIAL_WGSL: &str = include_str!("../wgsl/atmosphere_aerial.wgsl");

/// The uncomposed WGSL of a pass: view import, step counts, shared model, pass.
fn pass_source(pass: &str) -> String {
    // One source for the limb normalisation: `model::SUN_DISK_NORMALISATION`.
    let disk = format!("{:?}", model::SUN_DISK_NORMALISATION);
    format!(
        "#import engine::view
const SKY_STEPS: u32 = {SKY_STEPS}u;
const AERIAL_STEPS: u32 = {AERIAL_STEPS}u;
const SUN_STEPS: u32 = {SUN_STEPS}u;
const ATM_DISK_NORMALISATION: f32 = {disk};
{COMMON_WGSL}
{pass}"
    )
}

fn uniform_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<AtmosphereUniform>() as u64),
        },
        count: None,
    }
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

struct Gpu {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

fn create_gpu(
    setup: &FeatureSetup<'_>,
    label: &str,
    pass: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
    sky: bool,
) -> Result<Gpu> {
    let device = &setup.gpu.device;
    let source = setup
        .composer
        .compose(&pass_source(pass))
        .with_context(|| format!("composing {label}"))?;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&setup.layouts.view), Some(&layout)],
        immediate_size: 0,
    });
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let keep_alpha = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
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
        // The sky pass rasterizes at depth 1: only far-plane pixels pass.
        depth_stencil: sky.then_some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: sky.then_some(wgpu::BlendState {
                    color: add,
                    alpha: keep_alpha,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    Ok(Gpu { layout, pipeline })
}

fn uniform_buffer(ctx: &FeatureContext<'_>, label: &str, frame: &AtmosphereFrame) -> wgpu::Buffer {
    let buffer = ctx.gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: std::mem::size_of::<AtmosphereUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ctx.gpu.queue.write_buffer(
        &buffer,
        0,
        bytemuck::bytes_of(&AtmosphereUniform::new(frame)),
    );
    buffer
}

/// Sky, planet and sun disk added where depth is at the far plane.
pub struct AtmosphereFeature {
    controls: AtmosphereControls,
    gpu: Option<Gpu>,
}

impl AtmosphereFeature {
    pub fn new(controls: AtmosphereControls) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for AtmosphereFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(ATMOSPHERE_FEATURE, Stage::Sky).disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.gpu = Some(create_gpu(
            setup,
            "engine.atmosphere",
            SKY_WGSL,
            &[uniform_entry()],
            true,
        )?);
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
        if frame.settings.intensity == 0.0 {
            return Ok(());
        }
        let gpu = self.gpu.as_ref().context("atmosphere not prepared")?;
        let uniform = uniform_buffer(ctx, "engine.atmosphere", &frame);
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine.atmosphere"),
                layout: &gpu.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }],
            });
        let mut pass = ctx.scene_pass("engine.atmosphere sky")?;
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, ctx.view_bind_group, &[]);
        pass.set_bind_group(1, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

/// Atmospheric transmittance and in-scatter on opaque geometry.
pub struct AerialPerspectiveFeature {
    controls: AtmosphereControls,
    gpu: Option<Gpu>,
}

impl AerialPerspectiveFeature {
    pub fn new(controls: AtmosphereControls) -> Self {
        Self {
            controls,
            gpu: None,
        }
    }
}

impl PipelineFeature for AerialPerspectiveFeature {
    fn descriptor(&self) -> FeatureDescriptor {
        FeatureDescriptor::new(ATMOSPHERE_AERIAL_FEATURE, Stage::OpaqueEffects)
            // Fog the reflected image consistently with the surfaces it shows:
            // decals, then SSR, then aerial perspective.
            .after(crate::pipeline::names::SSR)
            .disabled_by_default()
    }

    fn prepare(&mut self, setup: &FeatureSetup<'_>) -> Result<()> {
        self.gpu = Some(create_gpu(
            setup,
            "engine.atmosphere.aerial",
            AERIAL_WGSL,
            &[
                uniform_entry(),
                texture_entry(1, false),
                texture_entry(2, true),
            ],
            false,
        )?);
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
        if frame.settings.aerial_perspective_scale == 0.0 || frame.settings.intensity == 0.0 {
            return Ok(());
        }
        let gpu = self
            .gpu
            .as_ref()
            .context("aerial perspective not prepared")?;
        let uniform = uniform_buffer(ctx, "engine.atmosphere.aerial", &frame);
        let depth = ctx.depth()?.view.clone();
        let (source, destination) = ctx.opaque_effect_io()?;
        let bind = ctx
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("engine.atmosphere.aerial"),
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
            label: Some("engine.atmosphere.aerial"),
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
mod tests {
    use super::*;

    fn bad(change: impl FnOnce(&mut AtmosphereSettings)) -> bool {
        let mut settings = AtmosphereSettings::default();
        change(&mut settings);
        settings.validate().is_err()
    }

    #[test]
    fn defaults_validate_and_physical_ranges_are_enforced() {
        AtmosphereSettings::default().validate().unwrap();
        assert!(bad(|s| s.planet_radius = f32::NAN));
        assert!(bad(|s| s.planet_radius = 10.0));
        assert!(bad(|s| s.planet_radius = 1.0e9));
        assert!(bad(|s| s.atmosphere_radius = s.planet_radius));
        assert!(bad(|s| s.atmosphere_radius = s.planet_radius + 10.0));
        assert!(bad(|s| s.atmosphere_radius = s.planet_radius + 2.0e7));
        assert!(bad(|s| s.rayleigh_scattering[1] = -1.0e-6));
        assert!(bad(|s| s.rayleigh_scattering[2] = f32::INFINITY));
        assert!(bad(|s| s.mie_scattering = 0.1));
        assert!(bad(|s| s.mie_absorption = f32::NAN));
        assert!(bad(|s| s.ozone_absorption[0] = -1.0));
        assert!(bad(|s| s.rayleigh_scale_height = 0.0));
        assert!(bad(|s| s.mie_scale_height = f32::INFINITY));
        assert!(bad(|s| s.mie_anisotropy = 1.0));
        assert!(bad(|s| s.mie_anisotropy = -0.995));
        assert!(bad(|s| s.ozone_width = 0.0));
        assert!(bad(|s| s.ozone_center_height = -1.0));
        assert!(bad(|s| s.ground_albedo[2] = 1.1));
        assert!(bad(|s| s.sun_angular_diameter = 0.6));
        assert!(bad(|s| s.sun_angular_diameter = -0.1));
        assert!(bad(|s| s.intensity = 1001.0));
        assert!(bad(|s| s.aerial_perspective_scale = f32::NAN));
        // Zero is a valid extreme: no medium, no disk, no aerial perspective.
        AtmosphereSettings {
            rayleigh_scattering: [0.0; 3],
            mie_scattering: 0.0,
            sun_angular_diameter: 0.0,
            aerial_perspective_scale: 0.0,
            ..Default::default()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn controls_keep_views_independent_and_reject_without_mutation() {
        let controls = AtmosphereControls::default();
        let (a, b) = (ViewId(3), ViewId(4));
        let frame = AtmosphereFrame::new(AtmosphereSettings::default(), 120.0).unwrap();
        controls.set(a, Some(frame)).unwrap();
        assert_eq!(controls.get(a).unwrap(), Some(frame));
        assert!(controls.get(b).unwrap().is_none());
        let invalid = AtmosphereFrame {
            settings: AtmosphereSettings {
                intensity: -1.0,
                ..Default::default()
            },
            camera_altitude: 0.0,
        };
        assert!(controls.set(a, Some(invalid)).is_err());
        assert_eq!(
            controls.get(a).unwrap(),
            Some(frame),
            "rejected set kept state"
        );
        assert!(AtmosphereFrame::new(AtmosphereSettings::default(), f64::NAN).is_err());
        controls.set(b, Some(frame)).unwrap();
        assert_eq!(controls.view_count(), 2);
        controls.set(a, None).unwrap();
        assert_eq!((controls.view_count(), controls.get(a).unwrap()), (1, None));
    }

    #[test]
    fn altitude_is_absolute_and_rebasing_invariant() {
        // The same absolute height under two different floating origins.
        let a = absolute_altitude(0.0, 5000.0);
        let b = absolute_altitude(4000.0, 1000.0);
        let c = absolute_altitude(-2_000_000.0, 2_005_000.0);
        assert_eq!((a, b, c), (5000.0, 5000.0, 5000.0));
        // A far origin keeps sub-metre precision that an f32 sum would lose.
        let far = absolute_altitude(1.0e9, 1.7);
        assert!((far - 1_000_000_001.7).abs() < 1e-6);
        let frame = AtmosphereFrame::new(AtmosphereSettings::default(), -3.0).unwrap();
        assert_eq!(frame.shader_altitude(), 0.0, "below the surface clamps");
    }

    #[test]
    fn uniform_layout_is_six_vec4s_and_carries_the_settings() {
        assert_eq!(std::mem::size_of::<AtmosphereUniform>(), 96);
        let settings = AtmosphereSettings {
            sun_angular_diameter: 0.02,
            intensity: 2.0,
            aerial_perspective_scale: 5.0,
            ..Default::default()
        };
        let frame = AtmosphereFrame::new(settings, 250.0).unwrap();
        let u = AtmosphereUniform::new(&frame);
        assert_eq!(u.rayleigh[3], 8000.0);
        assert_eq!(u.mie, [3.996e-6, 0.444e-6, 1200.0, 0.8]);
        assert_eq!(u.layer, [25_000.0, 15_000.0, 5.0, 0.01]);
        assert_eq!(u.ground[3], 2.0);
        assert_eq!(u.geometry, [6_360_000.0, 100_000.0, 250.0, 0.0]);
    }

    #[test]
    fn descriptors_are_disabled_by_default_in_the_right_stages() {
        let controls = AtmosphereControls::default();
        let mut registry = crate::builtin_registry().unwrap();
        register_features(&mut registry, &controls).unwrap();
        let sky = registry.descriptor(ATMOSPHERE_FEATURE).unwrap();
        let aerial = registry.descriptor(ATMOSPHERE_AERIAL_FEATURE).unwrap();
        assert_eq!(
            (sky.stage, aerial.stage),
            (Stage::Sky, Stage::OpaqueEffects)
        );
        assert!(!sky.enabled_by_default && !aerial.enabled_by_default);
        let names = registry.names();
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(at("engine.opaque") < at(ATMOSPHERE_FEATURE));
        assert!(at(ATMOSPHERE_FEATURE) < at(ATMOSPHERE_AERIAL_FEATURE));
        assert!(at(ATMOSPHERE_AERIAL_FEATURE) < at("engine.transparent"));
    }

    #[test]
    fn opaque_effects_run_decals_then_ssr_then_aerial_perspective() {
        let mut registry = crate::builtin_registry().unwrap();
        register_features(&mut registry, &AtmosphereControls::default()).unwrap();
        let names = registry.names();
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(at("engine.surface_material") < at("engine.ssr"));
        assert!(at("engine.decals") < at("engine.ssr"));
        assert!(at("engine.ssr") < at(ATMOSPHERE_AERIAL_FEATURE));
    }

    #[test]
    fn shaders_compose_and_validate() {
        let composer = crate::ShaderComposer::new();
        for pass in [SKY_WGSL, AERIAL_WGSL] {
            let source = composer.compose(&pass_source(pass)).unwrap();
            crate::validate_wgsl(&source).unwrap();
        }
    }
}
