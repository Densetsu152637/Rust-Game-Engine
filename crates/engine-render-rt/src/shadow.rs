//! RT shadow mask: per-pixel visibility of one directional light, hard or soft.

use crate::device::{Core, RtDevice};
use crate::error::{RtError, invalid};
use crate::kernel::{self, RtScalarFormat};
use crate::scene::RtScene;
use crate::settings::{MAX_RAYS_PER_PIXEL, MAX_SUN_ANGULAR_RADIUS, RtShadowSettings};
use crate::wgsl;
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;

/// Per-dispatch parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowMaskParams {
    /// Direction towards the light centre (normalized by the kernel).
    pub direction_to_light: [f32; 3],
    /// Offset of the ray origin along the surface normal, in world units.
    pub normal_bias: f32,
    pub t_min: f32,
    pub t_max: f32,
    /// Instance mask of occluders.
    pub mask: u8,
    /// Angular radius of the light disc in radians, `[0, 0.2]`. `0` traces
    /// one hard-shadow ray per pixel (visibility exactly 0 or 1); above `0`,
    /// `samples` rays are spread uniformly over the disc's cone and the
    /// visibility is the unoccluded fraction.
    pub angular_radius: f32,
    /// Rays per pixel for soft shadows, `[1, 64]`.
    pub samples: u32,
    /// Decorrelates soft-shadow noise between frames.
    pub frame_index: u32,
}

impl Default for ShadowMaskParams {
    fn default() -> Self {
        Self {
            direction_to_light: [0.0, 1.0, 0.0],
            normal_bias: 1e-3,
            t_min: 0.0,
            t_max: 1e4,
            mask: 0xff,
            angular_radius: 0.0,
            samples: 1,
            frame_index: 0,
        }
    }
}

impl ShadowMaskParams {
    /// Parameters for `settings`, the light direction and the frame index.
    pub fn from_settings(
        settings: &RtShadowSettings,
        direction_to_light: [f32; 3],
        frame_index: u32,
    ) -> Self {
        Self {
            direction_to_light,
            normal_bias: settings.normal_bias,
            t_max: settings.max_distance,
            angular_radius: settings.sun_angular_radius,
            samples: settings.samples_per_pixel,
            frame_index,
            ..Self::default()
        }
    }
}

/// Textures of one dispatch. `world_position` is `Rgba32Float` (xyz world
/// position, w >= 0.5 where there is geometry); `world_normal` is a float
/// texture whose xyz is the world normal (`Rgba16Float` or `Rgba32Float`);
/// `output` is a storage view of the kernel's format. All are `width x height`.
pub struct ShadowMaskTargets<'a> {
    pub world_position: &'a wgpu::TextureView,
    pub world_normal: &'a wgpu::TextureView,
    pub output: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuShadowParams {
    direction_to_light: [f32; 4],
    ray: [f32; 4],
    size_mask: [u32; 4],
    frame: [u32; 4],
}

const SHADOW_SEED: u32 = 0x5ad0_3e11;

fn gpu_params(
    width: u32,
    height: u32,
    params: &ShadowMaskParams,
) -> Result<GpuShadowParams, RtError> {
    let l = kernel::unit_direction("direction_to_light", params.direction_to_light)?;
    if !(params.normal_bias.is_finite()
        && params.normal_bias >= 0.0
        && params.t_min.is_finite()
        && params.t_max.is_finite()
        && params.t_min >= 0.0
        && params.t_max > params.t_min)
    {
        return Err(invalid(
            "normal_bias must be finite and >= 0, and 0 <= t_min < t_max finite",
        ));
    }
    if !(params.angular_radius.is_finite()
        && (0.0..=MAX_SUN_ANGULAR_RADIUS).contains(&params.angular_radius))
    {
        return Err(invalid(format!(
            "angular_radius {} must be within [0, {MAX_SUN_ANGULAR_RADIUS}]",
            params.angular_radius
        )));
    }
    if !(1..=MAX_RAYS_PER_PIXEL).contains(&params.samples) {
        return Err(invalid(format!(
            "samples {} must be within [1, {MAX_RAYS_PER_PIXEL}]",
            params.samples
        )));
    }
    // The shader traces the hard path when cos_max >= 1. In f32, cos(r)
    // rounds to 1 for r below about 3.5e-4 rad; such radii give penumbrae far
    // below a texel and are traced as hard shadows.
    let cos_max = params.angular_radius.cos();
    Ok(GpuShadowParams {
        direction_to_light: [l[0], l[1], l[2], cos_max],
        ray: [params.normal_bias, params.t_min, params.t_max, 0.0],
        size_mask: [width, height, u32::from(params.mask), params.samples],
        frame: [params.frame_index, SHADOW_SEED, 0, 0],
    })
}

/// Compute kernel `shadow_mask` (`src/shaders/shadow_mask.wgsl`), workgroup
/// 8 x 8, one invocation per pixel.
///
/// Bindings: group 0 = the scene ([`RtScene::bind_group`]); group 1 =
/// 0 `texture_2d<f32>` world positions, 1 `texture_2d<f32>` world normals
/// (both unfilterable float), 2 `texture_storage_2d<format, write>` visibility,
/// 3 uniform parameters. Hard shadows write 1 (lit) or 0 (shadowed); soft
/// shadows write the unoccluded fraction of `samples` rays in the light's
/// cone. Background pixels are 1 and surfaces facing away from the light
/// centre are 0.
pub struct ShadowMaskKernel {
    core: Arc<Core>,
    format: RtScalarFormat,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl ShadowMaskKernel {
    pub fn new(rt: &RtDevice, format: RtScalarFormat) -> Result<Self, RtError> {
        let core = rt.core().clone();
        core.ensure_usable()?;
        kernel::check_storage_format(rt.supports_storage_format(format.texture_format()), format)?;
        let device = &core.device;
        let layout = kernel::bind_group_layout(
            device,
            "engine-render-rt shadow mask",
            &[
                kernel::texture_entry(0),
                kernel::texture_entry(1),
                kernel::storage_texture_entry(2, format.texture_format()),
                kernel::uniform_entry(3),
            ],
        );
        let pipeline = kernel::compute_pipeline(
            device,
            "engine-render-rt shadow mask",
            &[&core.scene_layout, &layout],
            wgsl::shadow_mask_source(format.wgsl()),
            "shadow_mask",
        );
        Ok(Self {
            core,
            format,
            pipeline,
            layout,
        })
    }

    pub fn format(&self) -> RtScalarFormat {
        self.format
    }

    /// Records one dispatch into `encoder`. The scene must be committed.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        scene: &RtScene,
        targets: &ShadowMaskTargets<'_>,
        params: &ShadowMaskParams,
    ) -> Result<(), RtError> {
        let scene_group = kernel::scene_group(&self.core, scene)?;
        kernel::check_size(&self.core, "shadow mask", targets.width, targets.height)?;
        let uniform = gpu_params(targets.width, targets.height, params)?;
        let device = &self.core.device;
        let buffer = kernel::uniform_buffer(
            device,
            "engine-render-rt shadow params",
            bytemuck::bytes_of(&uniform),
        );
        let group = kernel::bind_group(
            device,
            "engine-render-rt shadow mask",
            &self.layout,
            &[
                wgpu::BindingResource::TextureView(targets.world_position),
                wgpu::BindingResource::TextureView(targets.world_normal),
                wgpu::BindingResource::TextureView(targets.output),
                buffer.as_entire_binding(),
            ],
        );
        kernel::dispatch(
            encoder,
            "engine-render-rt shadow mask",
            &self.pipeline,
            &[scene_group, &group],
            targets.width,
            targets.height,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_validated_and_soft_shadows_encode_the_cone() {
        assert_eq!(size_of::<GpuShadowParams>(), 64);
        let hard = gpu_params(4, 2, &ShadowMaskParams::default()).unwrap();
        assert_eq!(hard.direction_to_light, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(hard.size_mask, [4, 2, 0xff, 1]);
        let settings = RtShadowSettings {
            sun_angular_radius: 0.1,
            samples_per_pixel: 8,
            ..Default::default()
        };
        let soft = gpu_params(
            4,
            2,
            &ShadowMaskParams::from_settings(&settings, [0.0, 2.0, 0.0], 7),
        )
        .unwrap();
        assert!((soft.direction_to_light[3] - 0.1f32.cos()).abs() < 1e-7);
        assert_eq!(soft.size_mask[3], 8);
        assert_eq!(soft.frame[0], 7);
        let bad = |p: ShadowMaskParams| gpu_params(4, 2, &p).unwrap_err().to_string();
        assert!(
            bad(ShadowMaskParams {
                angular_radius: 0.5,
                ..Default::default()
            })
            .contains("angular_radius")
        );
        assert!(
            bad(ShadowMaskParams {
                samples: 0,
                ..Default::default()
            })
            .contains("samples")
        );
        assert!(
            bad(ShadowMaskParams {
                direction_to_light: [0.0; 3],
                ..Default::default()
            })
            .contains("direction_to_light")
        );
        assert!(
            bad(ShadowMaskParams {
                t_max: 0.0,
                ..Default::default()
            })
            .contains("t_max")
        );
    }
}
